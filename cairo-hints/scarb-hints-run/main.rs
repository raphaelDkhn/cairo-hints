use std::{
    env,
    fs::{self, File},
    io::BufReader,
    path::PathBuf,
};

use anyhow::{Context, Result};
use cairo_lang_sierra::program::VersionedProgram;
use cairo_oracle_hint_processor::{run_1, Error};
use camino::Utf8PathBuf;
use clap::Parser;
use itertools::Itertools;
use scarb_metadata::{MetadataCommand, PackageMetadata, ScarbCommand};
use scarb_ui::args::PackagesFilter;

mod deserialization;

/// Execute the main function of a package.
#[derive(Parser, Clone, Debug)]
#[command(author, version)]
struct Args {
    /// Name of the package.
    #[command(flatten)]
    packages_filter: PackagesFilter,

    /// Print more items in memory.
    #[arg(long, default_value_t = false)]
    print_full_memory: bool,

    /// Do not rebuild the package.
    #[arg(long, default_value_t = false)]
    no_build: bool,

    // #[clap(value_parser, value_hint=ValueHint::FilePath)]
    // filename: PathBuf,
    /// Input to the program.
    #[arg(default_value = "[]")]
    program_input: deserialization::Args,

    #[clap(long = "layout", default_value = "plain", value_parser=validate_layout)]
    layout: String,

    /// Maximum amount of gas available to the program.
    #[arg(long)]
    available_gas: Option<usize>,

    /// Oracle server URL.
    #[arg(long)]
    oracle_server: Option<String>,

    #[arg(long)]
    oracle_lock: Option<PathBuf>,

    #[clap(long = "trace_file", value_parser)]
    trace_file: Option<PathBuf>,

    #[structopt(long = "memory_file")]
    memory_file: Option<PathBuf>,
}

fn validate_layout(value: &str) -> Result<String, String> {
    match value {
        "plain"
        | "small"
        | "dex"
        | "starknet"
        | "starknet_with_keccak"
        | "recursive_large_output"
        | "all_cairo"
        | "all_solidity"
        | "dynamic" => Ok(value.to_string()),
        _ => Err(format!("{value} is not a valid layout")),
    }
}

fn main() -> Result<(), Error> {
    let args: Args = Args::parse();
    let metadata = MetadataCommand::new().inherit_stderr().exec().unwrap();
    let package = args.packages_filter.match_one(&metadata).unwrap();

    ScarbCommand::new().arg("build").run().unwrap();

    let filename = format!("{}.sierra.json", package.name);
    // println!("filename {:#?}", filename);
    let scarb_target_dir = env::var("SCARB_TARGET_DIR").unwrap();
    let scarb_profile = env::var("SCARB_PROFILE").unwrap();
    let path = Utf8PathBuf::from(scarb_target_dir.clone())
        .join(scarb_profile.clone())
        .join(filename.clone());

    // ensure!(
    //     path.exists(),
    //     formatdoc! {r#"
    //         package has not been compiled, file does not exist: {filename}
    //         help: run `scarb build` to compile the package
    //     "#}
    // );

    let lock_output = absolute_path(&package, args.oracle_lock, "oracle_lock", Some(PathBuf::from("Oracle.lock")))
        .expect("lock path must be provided either as an argument (--oracle-lock src) or in the Scarb.toml file in the [tool.hints] section.");
    let lock_file = File::open(lock_output).unwrap();
    let reader = BufReader::new(lock_file);
    let service_configuration = serde_json::from_reader(reader).unwrap();

    let sierra_program = serde_json::from_str::<VersionedProgram>(
        &fs::read_to_string(path.clone())
            .with_context(|| format!("failed to read Sierra file: {path}"))
            .unwrap(),
    )
    .with_context(|| format!("failed to deserialize Sierra program: {path}"))
    .unwrap()
    .into_v1()
    .with_context(|| format!("failed to load Sierra program: {path}"))
    .unwrap();

    let sierra_program = sierra_program.program;

    match run_1(
        &service_configuration,
        &args.oracle_server,
        &args.layout,
        &args.trace_file,
        &args.memory_file,
        &sierra_program,
        "::main",
    ) {
        Err(Error::Cli(err)) => err.exit(),
        Ok(return_values) => {
            if !return_values.is_empty() {
                let return_values_string_list =
                    return_values.iter().map(|m| m.to_string()).join(", ");
                println!("Return values : [{}]", return_values_string_list);
            }
            Ok(())
        }
        Err(Error::RunPanic(panic_data)) => {
            if !panic_data.is_empty() {
                let panic_data_string_list = panic_data
                    .iter()
                    .map(|m| {
                        // Try to parse to utf8 string
                        let msg = String::from_utf8(m.to_be_bytes().to_vec());
                        if let Ok(msg) = msg {
                            format!("{} ('{}')", m, msg)
                        } else {
                            m.to_string()
                        }
                    })
                    .join(", ");
                println!("Run panicked with: [{}]", panic_data_string_list);
            }
            Ok(())
        }
        Err(err) => Err(err),
    }
}

fn absolute_path(package: &PackageMetadata, arg: Option<PathBuf>, config_key: &str, default: Option<PathBuf>) -> Option<PathBuf> {
    let manifest_path = package.manifest_path.clone().into_std_path_buf();
    let project_dir = manifest_path.parent().unwrap();

    let definitions = arg.or_else(|| {
        package.tool_metadata("hints").and_then(|tool_config| {
            tool_config[config_key].as_str().map(PathBuf::from)
        })
    }).or(default)?;

    if definitions.is_absolute() {
        Some(definitions)
    } else {
        Some(project_dir.join(definitions))
    }
}
