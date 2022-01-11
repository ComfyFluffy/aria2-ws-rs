# aria2-ws

An aria2 websocket jsonrpc in Rust.

Built with `tokio`.

[Docs.rs](https://docs.rs/aria2_ws)

[aria2 RPC docs](https://aria2.github.io/manual/en/html/aria2c.html#methods)

## Features

- Almost all methods and structed responses
- Auto reconnect
- Ensures `on_complete` and `on_error` hook to be executed even after reconnected.
- Supports notifications

## Example

```rust
use std::sync::Arc;

use aria2_ws::{Client, TaskHooks, TaskOptions};
use futures::FutureExt;
use serde_json::json;
use tokio::{spawn, sync::Semaphore};

async fn example() {
    let client = Client::connect("ws://127.0.0.1:6800/jsonrpc", None)
        .await
        .unwrap();
    let options = TaskOptions {
        split: Some(2),
        header: Some(vec!["Referer: https://www.pixiv.net/".to_string()]),
        all_proxy: Some("http://127.0.0.1:10809".to_string()),
        // Add extra options which are not included in TaskOptions.
        extra_options: json!({"max-download-limit": "200K"})
            .as_object()
            .unwrap()
            .clone(),
        ..Default::default()
    };

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
            Some(TaskHooks {
                on_complete: Some({
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
            Some(TaskHooks {
                on_complete: Some({
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

    let mut not = client.subscribe_notifications();

    spawn(async move {
        loop {
            if let Ok(msg) = not.recv().await {
                println!("Received notification {:?}", &msg);
            } else {
                return;
            }
        }
    });

    // Wait for 2 tasks to finish.
    let _ = semaphore.acquire_many(2).await.unwrap();

    client.shutdown().await.unwrap();
}

```
