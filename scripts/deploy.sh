cargo build-arm
scp target/armv7-unknown-linux-gnueabihf/release/param_serv root@172.25.0.142:/root
scp target/armv7-unknown-linux-gnueabihf/release/examples/displayer root@172.25.0.142:/root
scp target/armv7-unknown-linux-gnueabihf/release/examples/randomizer root@172.25.0.142:/root
scp examples/params.txt root@172.25.0.142:/root
