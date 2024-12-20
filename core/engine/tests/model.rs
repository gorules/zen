use crate::support::test_data_root;
use std::fs;
use std::path::Path;
use zen_engine::model::DecisionContent;

mod support;

#[test]
#[cfg_attr(miri, ignore)]
fn jdm_serde() {
    let root_dir = test_data_root();
    let dir_entries = fs::read_dir(Path::new(root_dir.as_str())).unwrap();
    for maybe_dir_entry in dir_entries {
        let dir_entry = maybe_dir_entry.unwrap();
        let Ok(file_contents) = fs::read_to_string(dir_entry.path()) else {
            // We expect some directories to be skipped
            continue;
        };

        let serialized = serde_json::from_str::<DecisionContent>(&file_contents).unwrap();
        assert!(serde_json::to_string(&serialized).is_ok());
    }
}
