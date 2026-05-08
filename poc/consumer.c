// SPDX-License-Identifier: GPL-3.0-or-later

#include <stdlib.h>
#include <unistd.h>
#include <stdio.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <string.h>

#include <GLES3/gl3.h>
#include <GLFW/glfw3.h>

// Print GLFW errors to stderr instead of swallowing them silently. Set
// before glfwInit so init failures are visible too.
static void on_glfw_error(int code, const char *msg) {
	fprintf(stderr, "glfw error %d: %s\n", code, msg);
}

int main(void) {
	puts("consumer: hello");

	// --- Socket + SCM_RIGHTS code from steps 2 and 3 ---------------------
	// Disabled for step 4 (window-only). Re-enabled at step 8 when the
	// producer starts sending a real dmabuf fd over the socket.
#if 0
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

	char dummy;
	struct iovec iov = { .iov_base = &dummy, .iov_len = 1 };

	char cmsg_buf[CMSG_SPACE(sizeof(int))];
	memset(cmsg_buf, 0, sizeof(cmsg_buf));

	struct msghdr msg = {0};
	msg.msg_iov = &iov;
	msg.msg_iovlen = 1;
	msg.msg_control = cmsg_buf;
	msg.msg_controllen = sizeof(cmsg_buf);

	ssize_t n = recvmsg(client_fd, &msg, 0);
	if (n == -1) {
		perror("recvmsg");
		exit(EXIT_FAILURE);
	}

	struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
	if (!cmsg || cmsg->cmsg_level != SOL_SOCKET || cmsg->cmsg_type != SCM_RIGHTS) {
		fprintf(stderr, "no SCM_RIGHTS in received message\n");
		exit(EXIT_FAILURE);
	}

	int received_fd;
	memcpy(&received_fd, CMSG_DATA(cmsg), sizeof(int));

	printf("consumer: received fd %d (locally numbered)\n", received_fd);

	char buf[256] = {0};
	ssize_t r = read(received_fd, buf, sizeof(buf) - 1);
	if (r == -1) {
		perror("read from received fd");
		exit(EXIT_FAILURE);
	}

	printf("consumer: file contents (%zd bytes): %s\n", r, buf);

	close(received_fd);
	close(client_fd);
	close(sock_fd);
#endif

	// --- Step 4: open a GLFW window and clear it to a solid color --------
	// First GPU step. No producer involved yet — just proving that EGL +
	// GL ES work end-to-end on this machine before anything more complex
	// (textures in step 5, GBM in step 6, dmabuf import in step 8) goes in.

	glfwSetErrorCallback(on_glfw_error);

	if (!glfwInit()) {
		fprintf(stderr, "glfwInit failed\n");
		exit(EXIT_FAILURE);
	}

	// Ask for an OpenGL ES 3.0 context via EGL. ES + EGL is the path that
	// matters for veiland: EGL_EXT_image_dma_buf_import (step 8) is built
	// for ES, and Wayland/Mesa prefer EGL over GLX.
	glfwWindowHint(GLFW_CLIENT_API, GLFW_OPENGL_ES_API);
	glfwWindowHint(GLFW_CONTEXT_CREATION_API, GLFW_EGL_CONTEXT_API);
	glfwWindowHint(GLFW_CONTEXT_VERSION_MAJOR, 3);
	glfwWindowHint(GLFW_CONTEXT_VERSION_MINOR, 0);

	GLFWwindow *window = glfwCreateWindow(800, 600, "veiland consumer", NULL, NULL);
	if (!window) {
		fprintf(stderr, "glfwCreateWindow failed\n");
		glfwTerminate();
		exit(EXIT_FAILURE);
	}

	// Bind the window's GL context to this thread. Required before any
	// gl* call, including glClear below.
	glfwMakeContextCurrent(window);

	// Render loop: clear to blue, present, pump events, repeat. Exit when
	// the user closes the window (via WM, Esc isn't wired up).
	while (!glfwWindowShouldClose(window)) {
		glClearColor(0.2f, 0.4f, 0.8f, 1.0f);
		glClear(GL_COLOR_BUFFER_BIT);

		// Swap back buffer to the screen. Blocks until the next vsync by
		// default, so the loop runs at the monitor's refresh rate.
		glfwSwapBuffers(window);

		// Process pending window-system events (close button, resize, etc.).
		glfwPollEvents();
	}

	glfwDestroyWindow(window);
	glfwTerminate();

	return 0;
}
