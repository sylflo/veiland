# SPDX-License-Identifier: GPL-3.0-or-later
#
# M0 POC build. Produces two binaries: producer and consumer.
#
# This is throwaway code that validates cross-process DMA-BUF buffer
# sharing on the target hardware. It is not the foundation of veiland
# proper — M1 starts fresh in Rust.

CC      ?= cc
CFLAGS  ?= -std=c11 -Wall -Wextra -Wpedantic -O2 -g
LDFLAGS ?=

# Per-binary deps. The consumer needs a GL context + window for step 4
# onward; the producer is still socket-only until step 6 brings in GBM/EGL.
# glfw3 = window + EGL context, glesv2 = the gl* functions we actually call.
CONSUMER_PKGS    := glfw3 glesv2
CONSUMER_CFLAGS  := $(shell pkg-config --cflags $(CONSUMER_PKGS))
CONSUMER_LDFLAGS := $(shell pkg-config --libs   $(CONSUMER_PKGS))

BINS = producer consumer

.PHONY: all clean run-producer run-consumer

all: $(BINS)

producer: producer.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

consumer: consumer.c
	$(CC) $(CFLAGS) $(CONSUMER_CFLAGS) -o $@ $< $(LDFLAGS) $(CONSUMER_LDFLAGS)

run-producer: producer
	./producer

run-consumer: consumer
	./consumer

clean:
	rm -f $(BINS)
