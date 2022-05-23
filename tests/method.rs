use std::time::Duration;

use aria2_ws::{Client, TaskOptions};
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_with::{serde_as, skip_serializing_none};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

async fn test_global(c: &Client) -> Result<()> {
    let r = c.get_version().await?;
    println!("{:?}\n", r);

    let r = c.get_global_option().await?;
    println!("{:?}\n", r);

    let r = c.get_global_stat().await?;
    println!("{:?}\n", r);

    let r = c.get_global_option().await?;
    println!("{:?}\n", r);

    let r = c.get_session_info().await?;
    println!("{:?}\n", r);

    let r = c.tell_active().await?;
    println!("{:?}\n", r);

    let r = c.tell_stopped(0, 100).await?;
    println!("{:?}\n", r);

    let r = c.tell_waiting(0, 100).await?;
    println!("{:?}\n", r);

    Ok(())
}

async fn test_metadata(c: &Client, gid: &str) -> Result<()> {
    let r = c.get_option(&gid).await?;
    println!("{:?}\n", r);

    let r = c.get_files(&gid).await?;
    println!("{:?}\n", r);

    let r = c.get_peers(&gid).await?;
    println!("{:?}\n", r);

    let r = c.get_servers(&gid).await?;
    println!("{:?}\n", r);

    let r = c.get_uris(&gid).await?;
    println!("{:?}\n", r);

    Ok(())
}

async fn sleep(secs: u64) {
    tokio::time::sleep(Duration::from_secs(secs)).await;
}

#[tokio::test]
async fn global() -> Result<()> {
    let c = Client::connect("ws://localhost:6800/jsonrpc", None).await?;
    test_global(&c).await?;
    Ok(())
}

#[tokio::test]
async fn torrent() -> Result<()> {
    let c = Client::connect("ws://localhost:6800/jsonrpc", None).await?;

    let options = TaskOptions {
        max_download_limit: Some("100K".to_string()),
        extra_options: json!({
            "file-allocation": "none",
        })
        .as_object()
        .unwrap()
        .clone(),
        ..Default::default()
    };

    let gid = c
        .add_uri(vec!["magnet:?xt=urn:btih:9b4c1489bfccd8205d152345f7a8aad52d9a1f57&dn=archlinux-2022.05.01-x86_64.iso".to_string()], Some(options), None, None)
        .await?;

    sleep(10).await;

    // test_global(&c).await?;
    test_metadata(&c, &gid).await?;
    c.remove(&gid).await?;
    c.remove_download_result(&gid).await?;

    Ok(())
}

#[tokio::test]
async fn http() -> Result<()> {
    let c = Client::connect("ws://localhost:6800/jsonrpc", None).await?;

    let options = TaskOptions {
        max_download_limit: Some("100K".to_string()),
        extra_options: json!({
            "file-allocation": "none",
        })
        .as_object()
        .unwrap()
        .clone(),
        ..Default::default()
    };

    let gid = c
        .add_uri(
            vec![
                "https://mirror.hoster.kz/archlinux/iso/2022.04.05/archlinux-2022.04.05-x86_64.iso"
                    .to_string(),
            ],
            Some(options),
            None,
            None,
        )
        .await?;

    sleep(10).await;

    // test_global(&c).await?;
    test_metadata(&c, &gid).await?;
    c.remove(&gid).await?;
    c.remove_download_result(&gid).await?;

    Ok(())
}

use serde_with::DisplayFromStr;
#[serde_as]
#[skip_serializing_none]
#[derive(Deserialize, Serialize, Debug)]
struct A {
    #[serde_as(as = "Option<DisplayFromStr>")]
    #[serde(default)]
    a: Option<u32>,
    b: (i32, i32),
}

#[test]
fn serde_test() -> Result<()> {
    let a = A { a: None, b: (1, 2) };
    let j = serde_json::to_string(&a)?;
    println!("{}", j);
    let a = serde_json::from_str::<A>(&j)?;
    println!("{:?}", a);
    Ok(())
}
