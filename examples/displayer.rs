use param_serv::{connect, param_get};
use std::io::{self, BufReader, Write};
use std::thread;
use std::time::Duration;

fn main() -> io::Result<()> {
    let stream = connect();
    let mut w = stream.try_clone().expect("clone");
    let mut r = BufReader::new(stream);

    let mut cursor: u64 = 0;
    let mut changed: Vec<(String, f64)> = Vec::new();

    loop {
        changed.clear();
        param_get(&mut w, &mut r, &mut cursor, |name, val| {
            changed.push((name.to_owned(), val));
        })?;

        if !changed.is_empty() {
            print!("\x1b[2J\x1b[H");
            for (name, val) in &changed {
                println!("{}: {:.6}", name, val);
            }
            io::stdout().flush().ok();
        }

        thread::sleep(Duration::from_nanos(1_000_000_000 / 60));
    }
}
