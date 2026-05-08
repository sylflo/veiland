
// SPDX-License-Identifier: GPL-3.0-or-later

#include <stdlib.h>
#include <unistd.h>
#include <stdio.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <string.h>
#include <sys/types.h>
#include <fcntl.h>


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

	int file_fd = open("/tmp/scm-test.txt", O_RDONLY);
	if (file_fd == -1) {
		perror("open");
		exit(EXIT_FAILURE);
	}

	char dummy = 'X';
	struct iovec iov = { .iov_base = &dummy, .iov_len = 1 };

	char cmsg_buf[CMSG_SPACE(sizeof(int))];
	memset(cmsg_buf, 0, sizeof(cmsg_buf));


	struct msghdr msg = {0};
	msg.msg_iov = &iov;
	msg.msg_iovlen = 1;
	msg.msg_control = cmsg_buf;
	msg.msg_controllen = sizeof(cmsg_buf);

	struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
	cmsg->cmsg_level = SOL_SOCKET;
	cmsg->cmsg_type = SCM_RIGHTS;
	cmsg->cmsg_len = CMSG_LEN(sizeof(int));

	memcpy(CMSG_DATA(cmsg), &file_fd, sizeof(int));

	if (sendmsg(sock_fd, &msg, 0) == -1) {
		perror("sendmsg");
		exit(EXIT_FAILURE);
	}

	close(file_fd);

	return 0;
}
