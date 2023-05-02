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
