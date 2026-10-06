//! Inspect a user installed ORT runtime and optional WebGPU plugin without loading SAM graphs.
use anyhow::{bail, Result};
use ort::{environment::Environment, ep::ExecutionProvider};
fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.is_empty() || args.len()>2 {bail!("usage: discover <runtime-dylib> [plugin-dylib]");}
    let runtime=std::path::PathBuf::from(&args[0]).canonicalize()?;
    ort::init_from(&runtime)?.with_name("samts-provider-discovery").commit();
    let env=Environment::current()?;
    let plugin_handle=if let Some(arg)=args.get(1) {Some(env.register_ep_library("samts_discover",std::path::PathBuf::from(arg).canonicalize()?)?)} else {None};
    println!("runtime: {}",runtime.display());
    println!("builtin WebGPU available: {}",ort::ep::webgpu::WebGPU::default().is_available()?);
    for d in env.devices() {println!("device: {} / {} / {}",d.ep()?,d.ep_vendor()?,d.hardware_device().id());
        println!("hardware vendor: {:?}, type: {:?}", d.hardware_device().vendor(), d.hardware_device().ty());}
    if let Some(handle)=plugin_handle {handle.unregister()?;println!("plugin unregistered cleanly");}
    Ok(())
}
