use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = env::args_os().skip(1);
    let result = match args.next() {
        None => newvolim_decode_bench::benchmark_json(),
        Some(flag) if flag == "--chunk-dir" => {
            let root = args.next().ok_or("--chunk-dir requires a directory")?;
            if args.next().is_some() {
                return Err("unknown decode benchmark argument".into());
            }
            newvolim_decode_bench::benchmark_chunks_json(&read_chunks(Path::new(&root))?)
        }
        Some(_) => return Err("usage: newvolim-decode-bench [--chunk-dir DIRECTORY]".into()),
    }?;
    println!("{result}");
    Ok(())
}

fn read_chunks(root: &Path) -> Result<Vec<Vec<u8>>, Box<dyn std::error::Error>> {
    let mut paths = Vec::new();
    collect_files(root, &mut paths)?;
    paths.sort();
    let chunks: Result<Vec<_>, _> = paths.into_iter().map(fs::read).collect();
    Ok(chunks?)
}

fn collect_files(root: &Path, paths: &mut Vec<PathBuf>) -> Result<(), std::io::Error> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(&path, paths)?;
        } else if path.is_file() {
            paths.push(path);
        }
    }
    Ok(())
}
