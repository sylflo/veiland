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

# Per-binary deps.
#
# Consumer (glfw3 = window + EGL context, glesv2 = the gl* functions we call).
CONSUMER_PKGS    := glfw3 glesv2
CONSUMER_CFLAGS  := $(shell pkg-config --cflags $(CONSUMER_PKGS))
CONSUMER_LDFLAGS := $(shell pkg-config --libs   $(CONSUMER_PKGS))

# Producer (headless: gbm allocates GPU buffers, egl makes the GL context
# without a window, glesv2 supplies the gl* symbols).
PRODUCER_PKGS    := gbm egl glesv2
PRODUCER_CFLAGS  := $(shell pkg-config --cflags $(PRODUCER_PKGS))
PRODUCER_LDFLAGS := $(shell pkg-config --libs   $(PRODUCER_PKGS))

BINS = producer consumer

.PHONY: all clean run-producer run-consumer

all: $(BINS)

producer: producer.c
	$(CC) $(CFLAGS) $(PRODUCER_CFLAGS) -o $@ $< $(LDFLAGS) $(PRODUCER_LDFLAGS)

consumer: consumer.c
	$(CC) $(CFLAGS) $(CONSUMER_CFLAGS) -o $@ $< $(LDFLAGS) $(CONSUMER_LDFLAGS)

run-producer: producer
	./producer

run-consumer: consumer
	./consumer

clean:
	rm -f $(BINS)
