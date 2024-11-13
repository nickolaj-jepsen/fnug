use pyo3_stub_gen::{Result, StubInfo};

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().filter_or("RUST_LOG", "info")).init();
    let stub = stub_info();
    stub.generate()?;
    Ok(())
}

fn stub_info() -> StubInfo {
    let manifest_dir: &::std::path::Path = env!("CARGO_MANIFEST_DIR").as_ref();
    StubInfo::from_pyproject_toml(manifest_dir.parent().unwrap().join("pyproject.toml")).unwrap()
}
