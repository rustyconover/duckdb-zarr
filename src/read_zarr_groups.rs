use std::sync::atomic::{AtomicUsize, Ordering};

use duckdb::core::LogicalTypeId;
use duckdb::vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab};

use crate::selection::Selection;
use crate::zarr_reader::meta::{
    array_path_table, extract_file_system, list_array_names, open_store,
};
use crate::zarr_reader::tree::{self, StoreTree};
use crate::zarr_reader::types::Table;

#[derive(Debug, Clone)]
struct TableRow {
    group_path: String,
    schema_name: String,
    /// `None` when `read_zarr(store, group_path := ..., dims := ...)` cannot read
    /// this row as one table, or its name collides with another table's after
    /// case folding (see [`tree::table_names`] and `check_alignment`).
    table_name: Option<String>,
    dims: String,
    shape: String,
    chunk_shape: String,
    data_vars: String,
    coord_vars: String,
}

pub struct ReadZarrGroupsBind {
    rows: Vec<TableRow>,
}

unsafe impl Send for ReadZarrGroupsBind {}
unsafe impl Sync for ReadZarrGroupsBind {}

pub struct ReadZarrGroupsInit {
    /// The first row the next call emits. A DataTree store can have more
    /// tables than one output chunk holds, so rows go out in pages.
    next: AtomicUsize,
}

unsafe impl Send for ReadZarrGroupsInit {}
unsafe impl Sync for ReadZarrGroupsInit {}

pub struct ReadZarrGroupsVTab;

impl VTab for ReadZarrGroupsVTab {
    type BindData = ReadZarrGroupsBind;
    type InitData = ReadZarrGroupsInit;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn std::error::Error>> {
        bind.add_result_column("group_path", LogicalTypeId::Varchar.into());
        bind.add_result_column("schema_name", LogicalTypeId::Varchar.into());
        bind.add_result_column("table_name", LogicalTypeId::Varchar.into());
        bind.add_result_column("dims", LogicalTypeId::Varchar.into());
        bind.add_result_column("shape", LogicalTypeId::Varchar.into());
        bind.add_result_column("chunk_shape", LogicalTypeId::Varchar.into());
        bind.add_result_column("data_vars", LogicalTypeId::Varchar.into());
        bind.add_result_column("coord_vars", LogicalTypeId::Varchar.into());

        let store_path = bind.get_parameter(0).to_string();
        let fs = unsafe { extract_file_system(bind) };
        let store = open_store(&store_path, Some(fs))?;
        let array_names = list_array_names(&store_path, &store)?;
        let row = |node: &str, table_name: Option<String>, table: &Table| TableRow {
            group_path: tree::display_group(node),
            schema_name: tree::schema_name(node),
            table_name,
            dims: serde_json::to_string(&table.dims).unwrap_or_default(),
            shape: serde_json::to_string(&table.shape).unwrap_or_default(),
            chunk_shape: serde_json::to_string(&table.chunk_shape).unwrap_or_default(),
            data_vars: serde_json::to_string(&table.variable_paths()).unwrap_or_default(),
            coord_vars: serde_json::to_string(
                &table
                    .coords
                    .iter()
                    .map(|(_, path)| path)
                    .collect::<Vec<_>>(),
            )
            .unwrap_or_default(),
        };

        let mut rows = Vec::new();
        let (store_tree, nodes) = match Selection::from_bind(bind)? {
            Selection::Array(requested) => {
                // One array is not a table of its node, so it gets no table name.
                let (array_name, table) = array_path_table(&store, &array_names, &requested)?;
                rows.push(row(tree::node_of(&array_name), None, &table));
                return Ok(ReadZarrGroupsBind { rows });
            }
            Selection::Node(node) => {
                tree::ensure_group_exists(&store_path, &array_names, &node)?;
                let store_tree = StoreTree::load_for_node(&store, &array_names, &node)?;
                (store_tree, vec![node])
            }
            Selection::Store => {
                let store_tree = StoreTree::load(&store, &array_names, None)?;
                let nodes = store_tree.nodes();
                (store_tree, nodes)
            }
        };
        for node in nodes {
            // A row gets a table name only if read_zarr can read it as one table
            // (its node is aligned with its ancestors, no array of another
            // shape in the node shares its dims, and zarrs can open all its
            // arrays) and the name is unique in the node after case folding.
            let node_tables = store_tree.tables(&node);
            let aligned = node_tables.misaligned.is_none();
            let names = tree::table_names(&node_tables.tables);
            for (table, name) in node_tables.tables.iter().zip(names) {
                let readable = aligned && table.unreadable().is_empty();
                rows.push(row(&node, name.filter(|_| readable), table));
            }
        }

        Ok(ReadZarrGroupsBind { rows })
    }

    fn init(_: &InitInfo) -> Result<Self::InitData, Box<dyn std::error::Error>> {
        Ok(ReadZarrGroupsInit {
            next: AtomicUsize::new(0),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut duckdb::core::DataChunkHandle,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let bind = func.get_bind_data();
        let init = func.get_init_data();

        let vector_size = unsafe { duckdb::ffi::duckdb_vector_size() as usize };
        let start = init.next.fetch_add(vector_size, Ordering::Relaxed);
        if start >= bind.rows.len() {
            output.set_len(0);
            return Ok(());
        }
        let end = (start + vector_size).min(bind.rows.len());
        let n = end - start;

        let v_group = output.flat_vector(0);
        let v_schema = output.flat_vector(1);
        let mut v_table = output.flat_vector(2);
        let v_dims = output.flat_vector(3);
        let v_shape = output.flat_vector(4);
        let v_cshape = output.flat_vector(5);
        let v_dvars = output.flat_vector(6);
        let v_cvars = output.flat_vector(7);

        for (i, row) in bind.rows[start..end].iter().enumerate() {
            use duckdb::core::Inserter;
            v_group.insert(i, row.group_path.as_str());
            v_schema.insert(i, row.schema_name.as_str());
            match &row.table_name {
                Some(name) => v_table.insert(i, name.as_str()),
                None => v_table.set_null(i),
            }
            v_dims.insert(i, row.dims.as_str());
            v_shape.insert(i, row.shape.as_str());
            v_cshape.insert(i, row.chunk_shape.as_str());
            v_dvars.insert(i, row.data_vars.as_str());
            v_cvars.insert(i, row.coord_vars.as_str());
        }

        output.set_len(n);
        Ok(())
    }

    fn parameters() -> Option<Vec<duckdb::core::LogicalTypeHandle>> {
        Some(vec![LogicalTypeId::Varchar.into()])
    }

    fn named_parameters() -> Option<Vec<(String, duckdb::core::LogicalTypeHandle)>> {
        // Optional selectors: one node or one array. Without either, every
        // node's tables are listed.
        Some(Selection::named_parameters())
    }
}
