/*
 * Copyright (c) 2020-2021 Rob Patro, Avi Srivastava, Hirak Sarkar, Dongze He, Mohsen Zakeri.
 *
 * This file is part of alevin-fry
 * (see https://github.com/COMBINE-lab/alevin-fry).
 *
 * License: 3-clause BSD, see https://opensource.org/licenses/BSD-3-Clause
 */

extern crate bio_types;
extern crate chrono;
extern crate clap;
extern crate itertools;
extern crate num_cpus;
extern crate rand;
extern crate slog;
extern crate slog_term;

use bio_types::strand::Strand;
use clap::{crate_authors, crate_version, App, AppSettings, Arg, ArgSettings};
use csv::Error as CSVError;
use csv::ErrorKind;
use itertools::Itertools;
use mimalloc::MiMalloc;
use rand::Rng;
use slog::{crit, o, warn, Drain};

use alevin_fry::cellfilter::{generate_permit_list, CellFilterMethod};
use alevin_fry::quant::{ResolutionStrategy, SplicedAmbiguityModel};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// grab the version from the Cargo file.
const VERSION: &str = env!("CARGO_PKG_VERSION");

#[allow(dead_code)]
fn gen_random_kmer(k: usize) -> String {
    const CHARSET: &[u8] = b"ACGT";
    let mut rng = rand::thread_rng();
    let s: String = (0..k)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect();
    s
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let num_hardware_threads = num_cpus::get() as u32;
    let max_num_threads: String = (num_cpus::get() as u32).to_string();
    let max_num_collate_threads: String = (16_u32.min(num_hardware_threads).max(2_u32)).to_string();

    let crate_authors = crate_authors!("\n");
    let version = crate_version!();

    // capture the entire command line as a string
    let cmdline = std::env::args().join(" ");

    let convert_app = App::new("convert")
        .about("Convert a BAM file to a RAD file")
        .version(version)
        .author(crate_authors)
        .arg(Arg::from("-b, --bam=<bam-file> 'input SAM/BAM file'"))
        .arg(
            Arg::from("-t, --threads 'number of threads to use for processing'")
                .default_value(&max_num_threads),
        )
        .arg(Arg::from("-o, --output=<rad-file> 'output RAD file'"));

    let view_app = App::new("view")
        .about("View a RAD file")
        .version(version)
        .author(crate_authors)
        .arg(Arg::from("-r, --rad=<rad-file> 'input RAD file'"))
        .arg(
            Arg::from("-h, --header 'flag for printing header'")
                .takes_value(false)
                .required(false),
        )
        .arg(Arg::from("-o, --output=<rad-file> 'output plain-text-file file'").required(false));

    let gen_app = App::new("generate-permit-list")
        .about("Generate a permit list of barcodes from a RAD file")
        .version(version)
        .author(crate_authors)
        .arg(Arg::from("-i, --input=<input>  'input directory containing the map.rad RAD file'"))
        .arg(Arg::from("-d, --expected-ori=<expected-ori> 'the expected orientation of alignments'"))
        .arg(Arg::from(
            "-o, --output-dir=<output-dir>  'output directory'",
        ))
        .arg(Arg::from(
            "-k, --knee-distance  'attempt to determine the number of barcodes to keep using the knee distance method."
            ).conflicts_with_all(&["force-cell", "valid-bc", "expect-cells", "unfiltered-pl"])
        )
        .arg(Arg::from(
            "-e, --expect-cells=<expect-cells> 'defines the expected number of cells to use in determining the (read, not UMI) based cutoff'",
        ).conflicts_with_all(&["force-cells", "valid-bc", "knee-distance", "unfiltered-pl"])
        )
        .arg(Arg::from(
            "-f, --force-cells=<force-cells>  'select the top-k most-frequent barcodes, based on read count, as valid (true)'"
        ).conflicts_with_all(&["expect-cells", "valid-bc", "knee-distance", "unfiltered-pl"])
        )
        .arg(
            Arg::from(
                "-b, --valid-bc=<valid-bc> 'uses true barcode collected from a provided file'",
            )
            .conflicts_with_all(&["force-cells", "expect-cells", "knee-distance", "unfiltered-pl"]),
        )
        .arg(
            Arg::from(
                "-u, --unfiltered-pl=<unfiltered-pl> 'uses an unfiltered external permit list'",
            )
            .conflicts_with_all(&["force-cells", "expect-cells", "knee-distance", "valid-bc"])
            .requires("min-reads")
        )
        .arg(
            Arg::from("-m, --min-reads=<min-reads> 'minimum read count threshold; only used with --unfiltered-pl'")
                .default_value("10")
                .takes_value(true)
                .required(true));
    //.arg(Arg::from("-v, --velocity-mode 'flag for velocity mode'").takes_value(false).required(false));

    let collate_app = App::new("collate")
    .about("Collate a RAD file by corrected cell barcode")
    .version(version)
    .author(crate_authors)
    .arg(Arg::from("-i, --input-dir=<input-dir> 'input directory made by generate-permit-list'"))
    .arg(Arg::from("-r, --rad-dir=<rad-file> 'the directory containing the RAD file to be collated'"))
    .arg(Arg::from("-t, --threads 'number of threads to use for processing'").default_value(&max_num_collate_threads))
    .arg(Arg::from("-c, --compress 'compress the output collated RAD file'").takes_value(false).required(false))
    .arg(Arg::from("-m, --max-records=[max-records] 'the maximum number of read records to keep in memory at once'")
         .default_value("30000000"));
    //.arg(Arg::from("-e, --expected-ori=[expected-ori] 'the expected orientation of alignments'")
    //     .default_value("fw"));

    let quant_app = App::new("quant")
    .about("Quantify expression from a collated RAD file")
    .version(version)
    .author(crate_authors)
    .arg(Arg::from("-i, --input-dir=<input-dir>  'input directory containing collated RAD file'"))
    .arg(Arg::from("-m, --tg-map=<tg-map>  'transcript to gene map'"))
    .arg(Arg::from("-o, --output-dir=<output-dir> 'output directory where quantification results will be written'"))
    .arg(Arg::from("-t, --threads 'number of threads to use for processing'").default_value(&max_num_threads))
    .arg(Arg::from("-d, --dump-eqclasses 'flag for dumping equivalence classes'").takes_value(false).required(false))
    .arg(Arg::from("-b, --num-bootstraps 'number of bootstraps to use'").default_value("0"))
    .arg(Arg::from("--init-uniform 'flag for uniform sampling'").requires("num-bootstraps").takes_value(false).required(false))
    .arg(Arg::from("--summary-stat 'flag for storing only summary statistics'").requires("num-bootstraps").takes_value(false).required(false))
    .arg(Arg::from("--use-mtx 'flag for writing output matrix in matrix market instead of EDS'").takes_value(false).required(false))
    .arg(Arg::from("--quant-subset=<sfile> 'file containing list of barcodes to quantify, those not in this list will be ignored").required(false))
    .arg(Arg::from("-r, --resolution 'the resolution strategy by which molecules will be counted'")
        .possible_values(&["full", "trivial", "cr-like", "cr-like-em", "parsimony", "parsimony-em"])
        .case_insensitive(true))
    .arg(Arg::from("--sa-model 'preferred model of splicing ambiguity'")
        .possible_values(&["prefer-ambig", "winner-take-all"])
        .default_value("winner-take-all")
        .setting(ArgSettings::Hidden))
    .arg(Arg::from("--small-thresh 'cells with fewer than these many reads will be resolved using a custom approach'").default_value("10").setting(ArgSettings::Hidden));

    let infer_app = App::new("infer")
    .about("Perform inference on equivalence class count data")
    .version(version)
    .author(crate_authors)
    .arg(Arg::from("-c, --count-mat=<eqc-mat> 'matrix of cells by equivalence class counts'").takes_value(true).required(true))
    //.arg(Arg::from("-b, --barcodes=<barcodes> 'file containing the barcodes labeling the matrix rows'").takes_value(true).required(true))
    .arg(Arg::from("-e, --eq-labels=<eq-labels> 'file containing the gene labels of the equivalence classes'").takes_value(true).required(true))
    .arg(Arg::from("-o, --output-dir=<output-dir> 'output directory where quantification results will be written'").takes_value(true).required(true))
    .arg(Arg::from("-t, --threads 'number of threads to use for processing'").default_value(&max_num_threads))
    .arg(Arg::from("--usa 'flag specifying that input equivalence classes were computed in USA mode'").takes_value(false).required(false))
    .arg(Arg::from("--quant-subset=<sfile> 'file containing list of barcodes to quantify, those not in this list will be ignored").required(false))
    .arg(Arg::from("--use-mtx 'flag for writing output matrix in matrix market instead of EDS'").takes_value(false).required(false));

    let opts = App::new("alevin-fry")
        .setting(AppSettings::SubcommandRequiredElseHelp)
        .version(version)
        .author(crate_authors)
        .about("Process RAD files from the command line")
        .subcommand(gen_app)
        //.subcommand(test_app)
        .subcommand(collate_app)
        .subcommand(quant_app)
        .subcommand(infer_app)
        .subcommand(convert_app)
        .subcommand(view_app)
        .get_matches();

    let decorator = slog_term::TermDecorator::new().build();
    let drain = slog_term::CompactFormat::new(decorator)
        .use_custom_timestamp(|out: &mut dyn std::io::Write| {
            write!(out, "{}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S")).unwrap();
            Ok(())
        })
        .build()
        .fuse();
    let drain = slog_async::Async::new(drain).build().fuse();

    let log = slog::Logger::root(drain, o!());

    // You can handle information about subcommands by requesting their matches by name
    // (as below), requesting just the name used, or both at the same time
    /*
    if let Some(ref t) = opts.subcommand_matches("test") {
        let input_file: String = t.value_of_t("input").expect("no input string specified");
        let rad_dir: String = t.value_of_t("rad-dir").expect("no input string specified");
        let min_reads: usize = t
            .value_of_t("min-reads")
            .expect("min-reads must be a valid integer");
        if min_reads < 1 {
            crit!(
                log,
                "min-reads < 1 is not supported, the value {} was provided",
                min_reads
            );
            std::process::exit(1);
        }
        let _r = test_external_parse(input_file, rad_dir, min_reads, &log);
    }
    */

    if let Some(t) = opts.subcommand_matches("generate-permit-list") {
        let input_dir: String = t.value_of_t("input").expect("no input directory specified");
        let output_dir: String = t
            .value_of_t("output-dir")
            .expect("no input directory specified");

        let valid_ori: bool;
        let expected_ori = match t.value_of("expected-ori").unwrap().to_uppercase().as_str() {
            "RC" => {
                valid_ori = true;
                Strand::Reverse
            }
            "FW" => {
                valid_ori = true;
                Strand::Forward
            }
            "BOTH" => {
                valid_ori = true;
                Strand::Unknown
            }
            "EITHER" => {
                valid_ori = true;
                Strand::Unknown
            }
            _ => {
                valid_ori = false;
                Strand::Unknown
            }
        };

        if !valid_ori {
            crit!(
                log,
                "{} is not a valid option for --expected-ori",
                expected_ori
            );
            std::process::exit(1);
        }

        let mut fmeth = CellFilterMethod::KneeFinding;

        let _expect_cells: Option<usize> = match t.value_of_t("expect-cells") {
            Ok(v) => {
                fmeth = CellFilterMethod::ExpectCells(v);
                Some(v)
            }
            Err(_) => None,
        };

        if t.is_present("knee-distance") {
            fmeth = CellFilterMethod::KneeFinding;
        }

        let _force_cells = match t.value_of_t("force-cells") {
            Ok(v) => {
                fmeth = CellFilterMethod::ForceCells(v);
                Some(v)
            }
            Err(_) => None,
        };

        let _valid_bc = match t.value_of_t::<String>("valid-bc") {
            Ok(v) => {
                fmeth = CellFilterMethod::ExplicitList(v.clone());
                Some(v)
            }
            Err(_) => None,
        };

        //let _unfiltered_pl = match t.value_of_t::<String>("unfiltered-pl") {
        if let Ok(v) = t.value_of_t::<String>("unfiltered-pl") {
            let min_reads: usize = t
                .value_of_t("min-reads")
                .expect("min-reads must be a valid integer");
            if min_reads < 1 {
                crit!(
                    log,
                    "min-reads < 1 is not supported, the value {} was provided",
                    min_reads
                );
                std::process::exit(1);
            }
            fmeth = CellFilterMethod::UnfilteredExternalList(v, min_reads);
        };

        // velo_mode --- currently, on this branch, it is always false
        let velo_mode = false; //t.is_present("velocity-mode");

        match generate_permit_list(
            input_dir,
            output_dir,
            fmeth,
            expected_ori,
            VERSION,
            velo_mode,
            &cmdline,
            &log,
        ) {
            Ok(nc) if nc == 0 => {
                warn!(log, "found 0 corrected barcodes; please check the input.");
            }
            Err(e) => return Err(e),
            _ => (),
        };
    }

    // convert a BAM file, in *transcriptomic coordinates*, with
    // the appropriate barcode and umi tags, into a RAD file
    if let Some(t) = opts.subcommand_matches("convert") {
        let input_file: String = t.value_of_t("bam").unwrap();
        let rad_file: String = t.value_of_t("output").unwrap();
        let num_threads: u32 = t.value_of_t("threads").unwrap();
        alevin_fry::convert::bam2rad(input_file, rad_file, num_threads, &log)
    }

    // convert a rad file to a textual representation and write to stdout
    if let Some(t) = opts.subcommand_matches("view") {
        let rad_file: String = t.value_of_t("rad").unwrap();
        let print_header = t.is_present("header");
        let mut out_file: String = String::from("");
        if t.is_present("output") {
            out_file = t.value_of_t("output").unwrap();
        }
        alevin_fry::convert::view(rad_file, print_header, out_file, &log)
    }

    // collate a rad file to group together all records corresponding
    // to the same corrected barcode.
    if let Some(t) = opts.subcommand_matches("collate") {
        let input_dir: String = t.value_of_t("input-dir").unwrap();
        let rad_dir: String = t.value_of_t("rad-dir").unwrap();
        let num_threads = t.value_of_t("threads").unwrap();
        let compress_out = t.is_present("compress");
        let max_records: u32 = t.value_of_t("max-records").unwrap();
        alevin_fry::collate::collate(
            input_dir,
            rad_dir,
            num_threads,
            max_records,
            compress_out,
            &cmdline,
            VERSION,
            &log,
        )
        .expect("could not collate.");
    }

    // perform quantification of a collated rad file.
    if let Some(t) = opts.subcommand_matches("quant") {
        let num_threads = t.value_of_t("threads").unwrap();
        let num_bootstraps = t.value_of_t("num-bootstraps").unwrap();
        let init_uniform = t.is_present("init-uniform");
        let summary_stat = t.is_present("summary-stat");
        let dump_eq = t.is_present("dump-eqclasses");
        let use_mtx = t.is_present("use-mtx");
        let input_dir: String = t.value_of_t("input-dir").unwrap();
        let output_dir = t.value_of_t("output-dir").unwrap();
        let tg_map = t.value_of_t("tg-map").unwrap();
        let resolution: ResolutionStrategy = t.value_of_t("resolution").unwrap();
        let sa_model: SplicedAmbiguityModel = t.value_of_t("sa-model").unwrap();
        let small_thresh = t.value_of_t("small-thresh").unwrap();
        let filter_list = t.value_of("quant-subset");

        if dump_eq && (resolution == ResolutionStrategy::Trivial) {
            crit!(
                log,
                "\n\nGene equivalence classes are not meaningful in case of Trivial resolution."
            );
            std::process::exit(1);
        }

        if num_bootstraps > 0 {
            match resolution {
                ResolutionStrategy::CellRangerLikeEm | ResolutionStrategy::Full => {
                    // sounds good
                }
                _ => {
                    eprintln!(
                        "\n\nThe num_bootstraps argument was set to {}, but bootstrapping can only be used with the cr-like-em or full resolution strategies",
                        num_bootstraps
                    );
                    std::process::exit(1);
                }
            }
        }

        // first make sure that the input direcory passed in has the
        // appropriate json file in it.
        // we should take care to document this workflow explicitly.

        let parent = std::path::Path::new(&input_dir);
        let json_path = parent.join("generate_permit_list.json");

        // if the input directory contains the valid json file we want
        // then proceed.  otherwise print a critical error.
        if json_path.exists() {
            let velo_mode = alevin_fry::utils::is_velo_mode(input_dir.to_string());
            if velo_mode {
                match alevin_fry::quant::velo_quantify(
                    input_dir,
                    tg_map,
                    output_dir,
                    num_threads,
                    num_bootstraps,
                    init_uniform,
                    summary_stat,
                    dump_eq,
                    use_mtx,
                    resolution,
                    sa_model,
                    small_thresh,
                    filter_list,
                    &cmdline,
                    VERSION,
                    &log,
                ) {
                    // if we're all good; then great!
                    Ok(_) => {}
                    // if we have an error, see if it's an error parsing
                    // the CSV or something else.
                    Err(e) => match e.downcast_ref::<CSVError>() {
                        Some(error) => {
                            match *error.kind() {
                                // if a deserialize error, we already complained about it
                                ErrorKind::Deserialize { .. } => {
                                    return Err("execution terminated unexpectedly".into())
                                }
                                // if another type of error, just panic for now
                                _ => {
                                    panic!("could not quantify rad file.");
                                }
                            }
                        }
                        // if something else, just panic
                        None => {
                            panic!("could not quantify rad file.");
                        }
                    },
                }; // end match if
            } else {
                match alevin_fry::quant::quantify(
                    input_dir,
                    tg_map,
                    output_dir,
                    num_threads,
                    num_bootstraps,
                    init_uniform,
                    summary_stat,
                    dump_eq,
                    use_mtx,
                    resolution,
                    sa_model,
                    small_thresh,
                    filter_list,
                    &cmdline,
                    VERSION,
                    &log,
                ) {
                    // if we're all good; then great!
                    Ok(_) => {}
                    // if we have an error, see if it's an error parsing
                    // the CSV or something else.
                    Err(e) => match e.downcast_ref::<CSVError>() {
                        Some(error) => {
                            match *error.kind() {
                                // if a deserialize error, we already complained about it
                                ErrorKind::Deserialize { .. } => {
                                    return Err("execution terminated unexpectedly".into())
                                }
                                // if another type of error, just panic for now
                                _ => {
                                    panic!("could not quantify rad file.");
                                }
                            }
                        }
                        // if something else, just panic
                        None => {
                            panic!("could not quantify rad file.");
                        }
                    },
                }; //end quant if
            }; // end velo_mode if
        } else {
            crit!(log,
            "The provided input directory lacks a generate_permit_list.json file; this should not happen."
           );
        }
    } // end quant if

    // Given an input of equivalence class counts, perform inference
    // and output a target-by-cell count matrix.
    if let Some(t) = opts.subcommand_matches("infer") {
        let num_threads = t.value_of_t("threads").unwrap();
        let use_mtx = t.is_present("use-mtx");
        let output_dir = t.value_of_t("output-dir").unwrap();
        let count_mat = t.value_of_t("count-mat").unwrap();
        let eq_label_file = t.value_of_t("eq-labels").unwrap();
        let filter_list = t.value_of("quant-subset");
        let usa_mode = t.is_present("usa");
        //let bc_file = t.value_of_t("barcodes").unwrap();

        alevin_fry::infer::infer(
            //num_bootstraps,
            //init_uniform,
            //summary_stat,
            count_mat,
            eq_label_file,
            usa_mode,
            //bc_file,
            use_mtx,
            num_threads,
            filter_list,
            output_dir,
            &log,
        )
        .expect("could not perform inference from equivalence class counts.");
    }
    Ok(())
}
