use crate::support::test_data_root;
use std::fs;
use std::path::Path;
use zen_engine::model::DecisionContent;

mod support;

#[cfg(feature = "bincode")]
mod bincode_tests {
    use crate::support::load_test_data;
    use bincode::config;
    use zen_engine::model::DecisionContent;

    #[test]
    fn jdm_bincode() {
        let decision_content = load_test_data("table.json");
        let cache_slice_r = bincode::encode_to_vec(&decision_content, config::standard());

        assert!(cache_slice_r.is_ok(), "Bincode serialisation failed");

        let cache_slice = cache_slice_r.unwrap();
        let decode_res =
            bincode::decode_from_slice::<DecisionContent, _>(&cache_slice, config::standard());

        assert!(decode_res.is_ok(), "Bincode deserialization failed");

        let decoded_decision_content = decode_res.unwrap();
        assert_eq!(decoded_decision_content.0, decision_content);
    }
}

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
