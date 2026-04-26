#[test]
#[ignore = "requires KAG3 macro table parsing and macro expansion"]
fn conformance_kag_macro_expansion_fixture() {
    let source = include_str!("fixtures/conformance/runtime/macro_expansion.ks");

    assert!(!source.trim().is_empty());
    panic!("KAG macro expansion is not implemented yet");
}

#[test]
#[ignore = "requires KAG3 if/elsif/else/endif expression evaluation through TJS"]
fn conformance_kag_conditional_tags_fixture() {
    let source = include_str!("fixtures/conformance/runtime/conditional_tags.ks");

    assert!(!source.trim().is_empty());
    panic!("KAG conditional execution is not implemented yet");
}

#[test]
#[ignore = "requires KAG iscript/endscript integration with TJS runtime"]
fn conformance_kag_iscript_executes_tjs_fixture() {
    let source = include_str!("fixtures/conformance/runtime/iscript_executes_tjs.ks");

    assert!(!source.trim().is_empty());
    panic!("KAG iscript execution is not implemented yet");
}

#[test]
#[ignore = "requires KAG jump/call/return scenario control flow"]
fn conformance_kag_jump_call_return_fixture() {
    let source = include_str!("fixtures/conformance/runtime/jump_call_return.ks");

    assert!(!source.trim().is_empty());
    panic!("KAG jump/call/return control flow is not implemented yet");
}

#[test]
#[ignore = "requires KAG resource-backed scenario loading"]
fn conformance_kag_storage_resource_loading_fixture() {
    let source = include_str!("fixtures/conformance/runtime/storage_resource_loading.ks");

    assert!(!source.trim().is_empty());
    panic!("KAG storage resource loading is not implemented yet");
}

#[test]
#[ignore = "requires KAG message state, page breaks, and line breaks"]
fn conformance_kag_message_flow_fixture() {
    let source = include_str!("fixtures/conformance/runtime/message_flow.ks");

    assert!(!source.trim().is_empty());
    panic!("KAG message flow runtime is not implemented yet");
}
