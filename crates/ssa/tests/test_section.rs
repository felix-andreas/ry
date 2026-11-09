//! Code written ahead of time: compiled to an object, linked, encoded into a file at an offset,
//! mapped back and run; and the section parser fed arbitrary bytes.

use {
    ssa::{
        AbiParam, Builder, CallConv, MemFlagsData, ObjectModule, Signature,
        section::{self, Code},
        types::I64,
    },
    syntax::testing::{SplitMix64, for_each_seed},
};

static FLAG: i64 = 1000;

extern "C" fn add_seven(value: i64) -> i64 {
    value + 7
}

fn unary() -> Signature {
    let mut signature = Signature::new(CallConv::Host);
    signature.params.push(AbiParam::new(I64));
    signature.returns.push(AbiParam::new(I64));
    signature
}

/// `twice(x) = 2x`, and `entry(x) = add_seven(twice(x)) + FLAG`, which calls its sibling
/// relative to itself and reaches the helper and the flag through the link table.
fn build() -> Code {
    let mut module = ObjectModule::new().expect("object module");
    let twice = module.declare("twice", &unary()).expect("declare twice");
    let entry = module.declare("entry", &unary()).expect("declare entry");

    let mut builder = Builder::new(unary());
    let block = builder.create_block();
    builder.append_block_params_for_function_params(block);
    builder.switch_to_block(block);
    let value = builder.block_params(block)[0];
    let doubled = builder.ins().imul_imm_s(value, 2);
    builder.ins().return_(&[doubled]);
    builder.seal_all_blocks();
    module
        .define(twice, &builder.finalize())
        .expect("define twice");

    let mut builder = Builder::new(unary());
    let block = builder.create_block();
    builder.append_block_params_for_function_params(block);
    builder.switch_to_block(block);
    let value = builder.block_params(block)[0];
    let callee = builder.import_function(twice, unary());
    let call = builder.ins().call(callee, &[value]);
    let doubled = builder.inst_results(call)[0];
    let helper = builder.import_symbol("add_seven");
    let helper = builder.ins().symbol_addr(helper);
    let signature = builder.import_signature(unary());
    let call = builder.ins().call_indirect(signature, helper, &[doubled]);
    let added = builder.inst_results(call)[0];
    let flag = builder.import_symbol("flag");
    let flag = builder.ins().symbol_addr(flag);
    let flag = builder.ins().load(I64, MemFlagsData::trusted(), flag, 0);
    let total = builder.ins().iadd(added, flag);
    builder.ins().return_(&[total]);
    builder.seal_all_blocks();
    module
        .define(entry, &builder.finalize())
        .expect("define entry");

    let mut code = module
        .finish(&|name| matches!(name, "add_seven" | "flag"))
        .expect("link");
    code.metadata = b"front end".to_vec();
    code
}

#[test]
fn linked_code_runs_from_a_file_at_any_offset() {
    let code = build();
    assert_eq!(code.symbols.len(), 2);
    let resolve = |name: &str| match name {
        "add_seven" => Some(add_seven as *const () as usize),
        "flag" => Some(&FLAG as *const i64 as usize),
        _ => None,
    };
    let directory = std::env::temp_dir().join(format!("ry-section-{}", std::process::id()));
    std::fs::create_dir_all(&directory).expect("temporary directory");
    // A page-aligned offset maps the code from the file; an odd one copies it.
    for base in [0u64, 100, 4096 * 3 + 17] {
        let bytes = section::encode(&code, base);
        let mut file = vec![0xAB; base as usize];
        file.extend_from_slice(&bytes);
        let path = directory.join(format!("image-{base}"));
        std::fs::write(&path, &file).expect("write image");
        // SAFETY: the resolver gives the addresses the code was built against.
        let loaded = unsafe { section::load(&path, base, &bytes, &resolve) }
            .expect("load")
            .expect("this processor runs x86-64-v2 code");
        assert_eq!(loaded.metadata, b"front end");
        for name in ["twice", "entry"] {
            let address = loaded.functions[name];
            // SAFETY: both functions were built with the signature `i64 -> i64`.
            let function = unsafe {
                std::mem::transmute::<*const u8, extern "C" fn(i64) -> i64>(address as *const u8)
            };
            let expected = if name == "twice" { 42 } else { 1049 };
            assert_eq!(function(21), expected, "{name} at base {base}");
        }
    }
    std::fs::remove_dir_all(&directory).expect("remove temporary directory");
}

#[test]
fn unknown_symbols_fail_at_link_and_load() {
    let mut module = ObjectModule::new().expect("object module");
    let id = module.declare("f", &unary()).expect("declare");
    let mut builder = Builder::new(unary());
    let block = builder.create_block();
    builder.append_block_params_for_function_params(block);
    builder.switch_to_block(block);
    let symbol = builder.import_symbol("missing");
    let address = builder.ins().symbol_addr(symbol);
    builder.ins().return_(&[address]);
    builder.seal_all_blocks();
    module.define(id, &builder.finalize()).expect("define");
    assert!(module.finish(&|_| false).is_err());

    let code = build();
    let bytes = section::encode(&code, 0);
    // SAFETY: loading fails before any code runs.
    let loaded = unsafe { section::load(std::path::Path::new("/nonexistent"), 0, &bytes, &|_| None) };
    assert!(loaded.is_err());
}

#[test]
fn fuzz_section_parser_never_panics() {
    let valid = section::encode(&build(), 0);
    for_each_seed(2000, |seed| {
        let mut rng = SplitMix64(seed);
        let mut bytes = if rng.chance(1, 4) {
            (0..rng.below(256)).map(|_| rng.next_u64() as u8).collect()
        } else {
            valid.clone()
        };
        for _ in 0..1 + rng.below(8) {
            if bytes.is_empty() {
                break;
            }
            match rng.below(3) {
                0 => {
                    let index = rng.below(bytes.len().min(128));
                    bytes[index] = rng.next_u64() as u8;
                }
                1 => bytes.truncate(rng.below(bytes.len())),
                _ => {
                    let index = rng.below(bytes.len().min(64));
                    bytes[index] ^= 0x80;
                }
            }
        }
        if let Ok(header) = section::parse(&bytes) {
            assert!(header.code_offset + header.code.len() <= bytes.len());
            for &(_, offset) in &header.functions {
                assert!((offset as usize) < header.code.len());
            }
        }
    });
}
