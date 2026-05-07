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

BINS = producer consumer

.PHONY: all clean run-producer run-consumer

all: $(BINS)

producer: producer.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

consumer: consumer.c
	$(CC) $(CFLAGS) -o $@ $< $(LDFLAGS)

run-producer: producer
	./producer

run-consumer: consumer
	./consumer

clean:
	rm -f $(BINS)
