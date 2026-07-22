use crate::{Cli, RuntimeInfo, get_api};
use anyhow::Result;
use futures::future::try_join_all;
use querybuilder::{CliQuery, QueryType, command_to_query};
mod querybuilder;

pub async fn handle_cli_command(cli: Cli, rt: RuntimeInfo) -> Result<()> {
    let config = rt.config;
    match cli {
        // TODO: Block this action using type system.
        Cli {
            command: None,
            show_source: true,
            ..
        } => println!("Show source requires an associated API command"),
        Cli {
            command: None,
            input_json: Some(_),
            ..
        } => println!("API command must be provided when providing an input json file"),
        Cli {
            command: None,
            input_json: None,
            show_source: false,
        } => println!("No command provided"),
        Cli {
            command: Some(command),
            input_json: Some(input_array),
            show_source,
        } => {
            let source_futures = input_array.into_iter().map(tokio::fs::read_to_string);
            let sources = try_join_all(source_futures).await?;
            let cli_query = CliQuery {
                query_type: QueryType::FromSourceFiles(sources),
                show_source,
            };
            let api = get_api(&config).await?;
            let res = command_to_query(command, cli_query, api).await?;
            println!("{res}");
        }
        Cli {
            command: Some(command),
            input_json: None,
            show_source,
        } => {
            let cli_query = CliQuery {
                query_type: QueryType::FromApi,
                show_source,
            };
            let api = get_api(&config).await?;
            let res = command_to_query(command, cli_query, api).await?;
            println!("{res}");
        }
    }
    Ok(())
}

