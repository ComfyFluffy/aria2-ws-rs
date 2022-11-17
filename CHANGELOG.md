# Change Log

## 0.3.0

- Fix struct publicity for response structs
- Update `tokio-tungstenite` to `0.17`
- Methods that require GID are now using `&str`

## 0.4.0

- Refactor code to use channels instead of mutexes
- Remove default timeout
- Rename `Hooks` to `Callbacks`
