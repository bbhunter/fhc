use {
    clap::Parser,
    fhc::{args, httplib, structs::LibOptions, utils},
    std::{collections::HashSet, process::ExitCode},
    tokio::io::{self, AsyncReadExt},
};

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("{message}");
            ExitCode::FAILURE
        }
    }
}

/// Reads the hosts to check from standard input.
async fn hosts_from_stdin(suffix: Option<&str>) -> Result<HashSet<String>, String> {
    let mut buffer = String::new();
    io::stdin()
        .read_to_string(&mut buffer)
        .await
        .map_err(|e| format!("Error reading standard input: {e}"))?;

    Ok(buffer
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| match suffix {
            Some(domain) => format!("{line}.{domain}"),
            None => line.to_owned(),
        })
        .collect())
}

async fn run() -> Result<(), String> {
    let args = args::Cli::parse();

    let hosts = match (args.domain.as_deref(), args.bruteforce) {
        (Some(domain), true) => hosts_from_stdin(Some(domain)).await?,
        (Some(domain), false) => HashSet::from([domain.to_owned()]),
        (None, _) => hosts_from_stdin(None).await?,
    };

    if hosts.is_empty() {
        return Err("No hosts to check.".to_owned());
    }

    let lib_options = LibOptions {
        hosts,
        client: httplib::return_http_client(args.timeout, args.max_redirects),
        user_agents: utils::user_agents(),
        retries: args.retries,
        threads: args.threads,
        filter_codes: args.filter_codes,
        exclude_codes: args.exclude_codes,
        show_full_data: args.show_full_data,
        print_results: true,
        quiet_flag: args.quiet,
        ..LibOptions::default()
    };

    if !args.quiet && args.show_full_data {
        println!("DOMAIN,[FINAL_URL],[STATUS_CODE]");
    }

    httplib::return_http_data(&lib_options).await;
    Ok(())
}
