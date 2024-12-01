use fnug_core::stub_gen::stub_info;
use pyo3_stub_gen::Result;

fn main() -> Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().filter_or("RUST_LOG", "info")).init();
    let stub = stub_info();
    stub.generate()?;
    Ok(())
}
