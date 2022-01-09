# aria2-ws

An aria2 websocket RPC in Rust.

## Features

- Following [aria2 docs](https://aria2.github.io/manual/en/html/aria2c.html#methods)
- Almost all methods and structed responses
- Ensures `on_complete` and `on_error` hook to be executed even after reconnected.
- Supports notifications.
