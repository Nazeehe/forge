.PHONY: build test install

PREFIX ?= $(HOME)/.local

build:
	cargo build

test:
	cargo test

install:
	cargo build --release
	install -Dm755 target/release/forge $(DESTDIR)$(PREFIX)/bin/forge
