use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::Result;
use backon::{ExponentialBuilder, Retryable};
use bytes::Bytes;
use clap::Parser;
use lazy_regex::{regex, regex_captures};
use reqwest::{header::CONTENT_DISPOSITION, IntoUrl, Url};
use tokio::fs;
use tracing::{info, instrument, warn};

#[derive(Debug, Clone, clap::Parser)]
#[command(about, author, version)]
struct Args {
    #[arg(short, long, help = "output folder", default_value = ".")]
    output: PathBuf,
    #[arg(short, long, help = "delay each url request (in milisecond)")]
    delay: Option<u64>,
    #[arg(short, long, help = "Set referer header")]
    referer: Option<String>,
    #[arg(help = "file contains urls")]
    url_list: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();
    let urls = get_urls(&args.url_list).await?;
    let mut handles = Vec::new();
    for url in urls.into_iter() {
        let referer = args.referer.clone();
        let download_url = move || download_image(url.clone(), referer.clone());
        let handle = tokio::spawn(async move {
            download_url
                .retry(ExponentialBuilder::default().with_max_times(5))
                .await
        });
        handles.push(handle);
        if let Some(d) = args.delay {
            tokio::time::sleep(Duration::from_millis(d)).await;
        }
    }

    fs::create_dir_all(&args.output).await?;

    for (i, handle) in handles.into_iter().enumerate() {
        match handle.await {
            Ok(Ok((name, data))) => {
                fs::write(
                    args.output.join(name.unwrap_or(format!("file_{}", i))),
                    data,
                )
                .await?;
            }
            Ok(Err(e)) => warn!("{}", e),
            Err(e) => warn!("{}", e),
        }
    }

    Ok(())
}

#[instrument]
async fn download_image<T: IntoUrl + Debug>(
    url: T,
    referer: Option<String>,
) -> Result<(Option<String>, Bytes)> {
    let url = url.into_url()?;
    info!("Process url {}", url.to_string());
    let client = reqwest::Client::new();
    let mut request_builder = client.get(url.clone());
    request_builder = if let Some(r) = referer {
        request_builder.header("referer", r)
    } else {
        request_builder
    };
    let response = request_builder.send().await?.error_for_status()?;
    let headers = response.headers().clone();
    let file_name = if let Some(h) = headers.get(CONTENT_DISPOSITION) {
        if let Some((_, file_name)) = regex_captures!(r#"filename="(.*?)""#, h.to_str()?) {
            Some(file_name)
        } else {
            get_file_name_from_url(&url)
        }
    } else {
        get_file_name_from_url(&url)
    };
    let data = response.bytes().await?;
    Ok((file_name.map(|x| x.to_string()), data))
}

fn get_file_name_from_url(url: &Url) -> Option<&str> {
    url.path_segments().map(|s| s.last()).flatten()
}

async fn get_urls(path: &Path) -> Result<Vec<String>> {
    let content = fs::read_to_string(path).await?;
    let pattern = regex!(r#"(https?://\S+[.!,;\?'\"]?)\s"#);
    Ok(pattern
        .captures_iter(&content)
        .map(|c| c.get(1).unwrap().as_str().to_string())
        .collect())
}
