//! Build a bounded local raw-v3 OME-NGFF pyramid.
//!
//! This command intentionally accepts only an authorized local source and the raw `uint16`
//! subset understood by `newvolim-io`. It is a reproducible pyramid-generation boundary, not a
//! replacement for a streaming production encoder.

use std::{env, path::PathBuf};

use newvolim_io::{
    level_transform, read_v3_raw_u16_volume, write_v3_raw_u16_pyramid, DatasetSource,
    LocalSourcePolicy, RemoteSourcePolicy, SourceRegistry, MAX_REMOTE_ASSET_BYTES,
};

struct Arguments {
    input: PathBuf,
    output: PathBuf,
    max_size: u64,
}

fn main() -> Result<(), String> {
    let arguments = parse_arguments(env::args().skip(1))?;
    let source = open_local_source(&arguments.input)?;
    let metadata = source
        .read_dataset_metadata()
        .map_err(|error| format!("could not read input NGFF metadata: {error}"))?;
    let multiscale = metadata
        .multiscales
        .first()
        .ok_or_else(|| "input has no OME-NGFF multiscale".to_owned())?;
    if multiscale
        .axes
        .iter()
        .map(|axis| axis.name.as_str())
        .collect::<Vec<_>>()
        != ["z", "y", "x"]
    {
        return Err("the local raw-v3 pyramid command requires exactly Z,Y,X NGFF axes".into());
    }
    let transform = level_transform(multiscale, 0)
        .map_err(|error| format!("could not resolve level-zero transform: {error}"))?;
    let origin = transform
        .apply(&[0.0, 0.0, 0.0])
        .map_err(|error| format!("could not apply level-zero transform: {error}"))?;
    let mut spacing = [0.0; 3];
    for axis in 0..3 {
        let mut point = [0.0; 3];
        point[axis] = 1.0;
        let transformed = transform
            .apply(&point)
            .map_err(|error| format!("could not apply level-zero transform: {error}"))?;
        for component in 0..3 {
            let delta = transformed[component] - origin[component];
            if component == axis {
                spacing[axis] = delta;
            } else if delta.abs() > f64::EPSILON {
                return Err(
                    "the local raw-v3 pyramid command does not rewrite rotated or sheared transforms"
                        .into(),
                );
            }
        }
    }
    let array = source
        .read_array_info("0")
        .map_err(|error| format!("could not read level-zero array metadata: {error}"))?;
    let chunks: [usize; 3] = array
        .chunks
        .iter()
        .copied()
        .map(usize::try_from)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "level-zero chunks do not fit this platform".to_owned())?
        .try_into()
        .map_err(|_| {
            "the local raw-v3 pyramid command requires exactly three chunk axes".to_owned()
        })?;
    let volume = read_v3_raw_u16_volume(&source, "0", MAX_REMOTE_ASSET_BYTES)
        .map_err(|error| format!("could not read level-zero raw uint16 volume: {error}"))?;
    let levels = write_v3_raw_u16_pyramid(
        &arguments.output,
        &volume,
        spacing,
        chunks,
        arguments.max_size,
    )
    .map_err(|error| format!("could not write pyramid: {error}"))?;
    println!(
        "wrote {} raw-v3 OME-NGFF levels from {} to {}",
        levels.len(),
        arguments.input.display(),
        arguments.output.display()
    );
    Ok(())
}

fn parse_arguments(arguments: impl IntoIterator<Item = String>) -> Result<Arguments, String> {
    let mut input = None;
    let mut output = None;
    let mut max_size = 256;
    let mut arguments = arguments.into_iter();
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("{flag} requires a value"))?;
        match flag.as_str() {
            "--input" if input.is_none() => input = Some(PathBuf::from(value)),
            "--output" if output.is_none() => output = Some(PathBuf::from(value)),
            "--max-size" => {
                max_size = value
                    .parse()
                    .map_err(|_| "--max-size must be a positive integer".to_owned())?;
                if max_size == 0 {
                    return Err("--max-size must be a positive integer".into());
                }
            }
            "--input" | "--output" => return Err(format!("{flag} may be specified only once")),
            _ => {
                return Err(format!(
                    "unknown option {flag}; use --input SOURCE --output DEST [--max-size N]"
                ))
            }
        }
    }
    Ok(Arguments {
        input: input.ok_or_else(|| "--input SOURCE is required".to_owned())?,
        output: output.ok_or_else(|| "--output DEST is required".to_owned())?,
        max_size,
    })
}

fn open_local_source(root: &PathBuf) -> Result<DatasetSource, String> {
    let parent = root
        .parent()
        .ok_or_else(|| format!("input root {} has no parent directory", root.display()))?;
    let registry = SourceRegistry::new(
        LocalSourcePolicy::new(vec![parent.to_path_buf()])
            .map_err(|error| format!("invalid local source policy: {error}"))?,
        RemoteSourcePolicy::default(),
        MAX_REMOTE_ASSET_BYTES,
    )
    .map_err(|error| format!("invalid source registry: {error}"))?;
    registry
        .open_local(root)
        .map_err(|error| format!("local source rejected: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_requires_single_local_input_and_positive_max_size() {
        assert!(
            parse_arguments(["--input", "in.zarr", "--output", "out.zarr"].map(str::to_owned))
                .is_ok()
        );
        assert!(parse_arguments(
            [
                "--input",
                "in.zarr",
                "--output",
                "out.zarr",
                "--max-size",
                "0"
            ]
            .map(str::to_owned)
        )
        .is_err());
        assert!(parse_arguments(["--input", "in.zarr"].map(str::to_owned)).is_err());
    }
}
