# ParamServ

A small parameter server for embedded Linux instruments. Firmware processes can
write and read Parametesr via a Unix domain socket; browser UIs can read them
via HTTP/SSE.

## Quick start

Define your parameters in a text file:

```
sensitivity   1.0
time_constant 0.1
frequency     1000.0
```

Run the server:

```sh
cargo run --release -- params.txt
```

From another Rust process, use the crate API:

```rust
use param_serv::{connect, param_set, param_get};

let sock = connect(); // blocks until server is up
let mut w = sock.try_clone().unwrap();
let mut r = std::io::BufReader::new(sock);

param_set(&mut w, &mut r, &[("frequency", 440.0)]).unwrap();

let mut cursor = 0u64;
param_get(&mut w, &mut r, &mut cursor, |name, val| {
    println!("{name} = {val}");
}).unwrap();
```

From a browser, `GET /events` on port 7777 gives an SSE stream:

```
data: {"c":4,"p":{"sensitivity":1.0,"frequency":440.0,...}}
```

See `examples/` for a debug UI and a randomizer.

### Author

Jakob Kastelic (Stanford Research Systems)
