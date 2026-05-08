// SPDX-License-Identifier: GPL-3.0-or-later

#include <stdlib.h>
#include <unistd.h>
#include <stdio.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <string.h>

int main(void) {
	puts("consumer: hello");
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
	unlink(addr.sun_path);

	if (bind(sock_fd, (struct sockaddr *)&addr, sizeof(addr)) == -1) {                                                                                                                                                                                                                                        
		perror("bind");
		exit(EXIT_FAILURE);                                                                                                                                                                                                                                                                              
	}

	if (listen(sock_fd, 1) == -1) {
		perror("listen");
		exit(EXIT_FAILURE);
	}

	int client_fd = accept(sock_fd, NULL, NULL);
	if (client_fd == -1) {
		perror("accept");
		exit(EXIT_FAILURE);
	}
	printf("Accepted new connection on client socket fd: %d\n", client_fd);

	char buf[256] = {0};
	ssize_t n = read(client_fd, buf, sizeof(buf) - 1);
	if (n == -1) {
		perror("read");
		exit(EXIT_FAILURE);
	}
	printf("Consumer: got %zd bytes: %s\n", n, buf);

	close(client_fd);
	close(sock_fd);


	return 0;
}
