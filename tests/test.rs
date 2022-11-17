use std::{sync::Arc, time::Duration};

use aria2_ws::{Callbacks, Client, TaskOptions};
use futures::{FutureExt, StreamExt};
use serde_json::json;
use tokio::{
    spawn,
    sync::{broadcast, Semaphore},
    time::sleep,
};

#[tokio::test]
#[ignore]
async fn drop_test() {
    Client::connect("ws://127.0.0.1:6800/jsonrpc", None)
        .await
        .unwrap();
}

#[tokio::test]
// #[ignore]
async fn example() {
    let client = Client::connect("ws://127.0.0.1:6800/jsonrpc", None)
        .await
        .unwrap();
    let options = TaskOptions {
        split: Some(2),
        header: Some(vec!["Referer: https://www.pixiv.net/".to_string()]),
        // Add extra options which are not included in TaskOptions.
        extra_options: json!({"max-download-limit": "100K"})
            .as_object()
            .unwrap()
            .clone(),
        ..Default::default()
    };

    let mut not = client.subscribe_notifications();
    spawn(async move {
        loop {
            match not.recv().await {
                Ok(msg) => println!("Received notification {:?}", &msg),
                Err(broadcast::error::RecvError::Closed) => {
                    println!("Notification channel closed");
                    break;
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    println!("Notification channel lagged");
                }
            }
        }
    });

    // use `tokio::sync::Semaphore` to wait for all tasks to finish.
    let semaphore = Arc::new(Semaphore::new(0));
    client
        .add_uri(
            vec![
                "https://i.pximg.net/img-original/img/2020/05/15/06/56/03/81572512_p0.png"
                    .to_string(),
            ],
            Some(options.clone()),
            None,
            Some(Callbacks {
                on_download_complete: Some({
                    let s = semaphore.clone();
                    async move {
                        s.add_permits(1);
                        println!("Task 1 completed!");
                    }
                    .boxed()
                }),
                on_error: Some({
                    let s = semaphore.clone();
                    async move {
                        s.add_permits(1);
                        println!("Task 1 error!");
                    }
                    .boxed()
                }),
            }),
        )
        .await
        .unwrap();

    // Will 404
    client
        .add_uri(
            vec![
                "https://i.pximg.net/img-original/img/2022/01/05/23/32/16/95326322_p0.pngxxxx"
                    .to_string(),
            ],
            Some(options.clone()),
            None,
            Some(Callbacks {
                on_download_complete: Some({
                    let s = semaphore.clone();
                    async move {
                        s.add_permits(1);
                        println!("Task 2 completed!");
                    }
                    .boxed()
                }),
                on_error: Some({
                    let s = semaphore.clone();
                    async move {
                        s.add_permits(1);
                        println!("Task 2 error!");
                    }
                    .boxed()
                }),
            }),
        )
        .await
        .unwrap();

    // Wait for 2 tasks to finish.
    let _ = semaphore.acquire_many(2).await.unwrap();

    // client.shutdown().await.unwrap();
}

#[tokio::test]
async fn test_broadcast() {
    let (tx, mut _rx) = broadcast::channel::<String>(2);
    let _ = tx.send("test out 0".to_string());
    {
        let tx = tx.clone();
        spawn(async move {
            sleep(Duration::from_secs(1)).await;
            tx.send("test".to_string()).unwrap();
            sleep(Duration::from_secs(1)).await;
            tx.send("test2".to_string()).unwrap();
        });
    }

    // let mut rx = tx.subscribe();
    drop(tx);
    spawn(async move {
        loop {
            sleep(Duration::from_millis(100)).await;
            let r = _rx.recv().await;
            println!("Received notification {:?}", r);
            match r {
                Ok(_) => {}
                Err(broadcast::error::RecvError::Closed) => {
                    break;
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
}
