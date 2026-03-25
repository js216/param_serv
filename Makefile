.PHONY: all clean

all:
	cargo build --bins --examples

clean:
	rm -rf target
