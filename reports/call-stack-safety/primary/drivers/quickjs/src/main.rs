#![forbid(unsafe_code)]

use rquickjs::function::Func;
use rquickjs::{CatchResultExt, Context, Function, Module, Runtime};
use std::time::Instant;

fn main() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Err("usage: driver PACKAGE_JS DRIVER_JS CONFIG_JSON_FILE".into());
    }
    let package = std::fs::read(&args[1]).map_err(|e| e.to_string())?;
    let driver = std::fs::read(&args[2]).map_err(|e| e.to_string())?;
    let config = std::fs::read_to_string(&args[3]).map_err(|e| e.to_string())?;
    // Match the repository runner's pinned rquickjs runtime and default limits.
    // No stack enlargement, cache lookup, compiler or process runner is involved.
    let runtime = Runtime::new().map_err(|e| e.to_string())?;
    let context = Context::full(&runtime).map_err(|e| e.to_string())?;
    let epoch = Instant::now();
    context.with(|ctx| -> Result<(), String> {
        let result = (|| -> rquickjs::Result<String> {
            let module = Module::declare(ctx.clone(), "bench-package", package)?;
            let (module, promise) = module.eval()?;
            promise.finish::<()>()?;
            let factory: Function = module.get("createBench")?;
            ctx.globals().set("benchFactory", factory)?;
            ctx.globals().set("benchConfigJson", config)?;
            ctx.globals().set(
                "benchClockNs",
                Func::from(move || epoch.elapsed().as_nanos() as f64),
            )?;
            ctx.eval(driver)
        })();
        let output = result.catch(&ctx).map_err(|e| format!("{e:?}"))?;
        println!("{output}");
        Ok(())
    })
}
