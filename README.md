# ParamServ

A small parameter server for embedded Linux instruments. Firmware processes can
write and read Parametesr via a Unix domain socket; browser UIs can read them
via HTTP/SSE.

## Quick Start

Define your parameters in a text file:

```
sensitivity   1.0
time_constant 0.1
frequency     1000.0
```

Run the server:

```sh
cargo run -- params.txt
```

See `examples/` for code that connects to the server to read and write
parameters via:

- Unix-Domain Sockets: `displayer.rs`, `randomizer.rs`
- Server-Sent Events: `debug.html`

### Author

Jakob Kastelic (Stanford Research Systems)
