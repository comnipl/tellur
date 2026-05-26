use std::env;
use std::error::Error;
use std::path::PathBuf;

use tellur_core::raster::Resolution;
use tellur_live::{serve, ServerOptions};

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || matches!(args[0].as_str(), "-h" | "--help") {
        println!("{}", usage());
        return Ok(());
    }
    let options = parse_args(args.into_iter())?;
    serve(options)
}

fn parse_args(mut args: impl Iterator<Item = String>) -> Result<ServerOptions, Box<dyn Error>> {
    let Some(command) = args.next() else {
        return Err(usage().into());
    };
    if command != "serve" {
        return Err(usage().into());
    }

    let mut plugin_path: Option<PathBuf> = None;
    let mut bind: Option<String> = None;
    let mut host = "127.0.0.1".to_owned();
    let mut port = 4317u16;
    let mut resolution = Resolution::new(1280, 720);
    let mut fps = 30u32;
    let mut verbose = false;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--plugin" => {
                plugin_path = Some(PathBuf::from(
                    args.next().ok_or("--plugin requires a path")?,
                ));
            }
            "--bind" => {
                bind = Some(args.next().ok_or("--bind requires an address")?);
            }
            "--host" => {
                host = args.next().ok_or("--host requires an address")?;
            }
            "--port" => {
                port = args
                    .next()
                    .ok_or("--port requires a value")?
                    .parse()
                    .map_err(|_| "--port must be a TCP port")?;
            }
            "--size" => {
                resolution = parse_resolution(&args.next().ok_or("--size requires WxH")?)?;
            }
            "--fps" => {
                fps = args
                    .next()
                    .ok_or("--fps requires a value")?
                    .parse()
                    .map_err(|_| "--fps must be a positive integer")?;
                if fps == 0 {
                    return Err("--fps must be greater than zero".into());
                }
            }
            "--verbose" => {
                verbose = true;
            }
            "-h" | "--help" => return Err(usage().into()),
            other if plugin_path.is_none() => plugin_path = Some(PathBuf::from(other)),
            other => return Err(format!("unknown argument: {other}\n\n{}", usage()).into()),
        }
    }

    Ok(ServerOptions {
        plugin_path: plugin_path.ok_or_else(usage)?,
        bind: bind.unwrap_or_else(|| format!("{host}:{port}")),
        resolution,
        fps,
        verbose,
    })
}

fn parse_resolution(s: &str) -> Result<Resolution, Box<dyn Error>> {
    let (w, h) = s.split_once('x').ok_or("resolution must be WIDTHxHEIGHT")?;
    Ok(Resolution::new(w.parse()?, h.parse()?))
}

fn usage() -> String {
    "usage: tellur-live serve --plugin <path-to-cdylib> [--host 127.0.0.1] [--port 4317] [--bind 127.0.0.1:4317] [--size 1280x720] [--fps 30] [--verbose]".to_owned()
}
