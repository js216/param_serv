use param_serv::{connect, param_get};
use std::io::{self, BufReader};

fn main() -> io::Result<()> {
    let stream = connect();
    let mut w = stream.try_clone().expect("clone");
    let mut r = BufReader::new(stream);

    let mut cursor: u64 = 0;

    loop {
        let changed = param_get(&mut w, &mut r, &mut cursor)?;
        print!("\x1b[2J\x1b[H");
        for (name, val) in &changed {
            println!("{}: {:.6}", name, val);
        }
    }
}
