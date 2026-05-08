
// SPDX-License-Identifier: GPL-3.0-or-later

#include <stdlib.h>
#include <unistd.h>
#include <stdio.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <string.h>


int main(void) {
	puts("producer: hello");

	// Create socket
	struct sockaddr_un addr;
	int sock_fd = socket(AF_UNIX, SOCK_STREAM, 0);
	
    if (sock_fd == -1) {
        perror("socket failed");
        exit(EXIT_FAILURE);
    }
	memset(&addr, 0, sizeof(addr));
	addr.sun_family = AF_UNIX;
	strncpy(addr.sun_path, "/tmp/veiland-poc.sock", sizeof(addr.sun_path) - 1);

	if (connect(sock_fd, (struct sockaddr *)&addr, sizeof addr) == -1) {
        perror("connect failed");
        exit(EXIT_FAILURE);
	}
	printf("Connected\n");

	const char *msg = "Hello from producer";
	ssize_t written = write(sock_fd, msg, strlen(msg));
	if (written == -1) {
		perror("write");
		exit(EXIT_FAILURE);
	}
	printf("Producer: sent %zd bytes", written);

	return 0;
}
