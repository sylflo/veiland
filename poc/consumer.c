// SPDX-License-Identifier: GPL-3.0-or-later

#include <stdlib.h>
#include <unistd.h>
#include <stdio.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <string.h>

#include <GLES3/gl3.h>
#include <GLFW/glfw3.h>
#include <stdint.h>
#include <EGL/egl.h> 
#include <EGL/eglext.h>
#include <GLES2/gl2ext.h>

struct buffer_msg {
	uint32_t width;
	uint32_t height;
	uint32_t format;
	uint32_t stride;
	uint64_t modifier;
};

const char *vertexShaderSource = "#version 300 es\n"
	"layout (location = 0) in vec3 aPos;\n"
	"layout (location = 1) in vec2 aUV;\n"
	"out vec2 vUV;\n"
	"void main()\n"
	"{\n"
	"	gl_Position = vec4(aPos.x, aPos.y, aPos.z, 1.0);\n"
	"	vUV = aUV;\n"
	"}\0";

const char *fragmentShaderSource = "#version 300 es\n"
	"precision mediump float;\n"
    "out vec4 FragColor;\n"
	"in vec2 vUV;\n"
	"uniform sampler2D uTex;\n"
    "void main()\n"
    "{\n"
    "   FragColor = texture(uTex, vUV);\n"
    "}\n\0";

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

	puts("Consumer: waiting for producer...");
	int client_fd = accept(sock_fd, NULL, NULL);
	if (client_fd == -1) {
		perror("accept");
		exit(EXIT_FAILURE);
	}
	printf("Accepted new connection on client socket fd: %d\n", client_fd);

	struct buffer_msg meta;
	struct iovec iov = { .iov_base = &meta, .iov_len = sizeof(meta) };

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

	int dmabuf_fd;
	memcpy(&dmabuf_fd, CMSG_DATA(cmsg), sizeof(int));

	printf("consumer: got fd=%d, %ux%u, format=0x%x, stride=%u, modifier=0x%lx\n", 
		dmabuf_fd, meta.width, meta.height, meta.format, meta.stride,
		 (unsigned long)meta.modifier);

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


	close(client_fd);
	close(sock_fd);


	// VertexShader
	unsigned int vertexShader;
	vertexShader = glCreateShader(GL_VERTEX_SHADER);
	glShaderSource(vertexShader, 1, &vertexShaderSource, NULL);
	glCompileShader(vertexShader);
    int success;
    char infoLog[512];
    glGetShaderiv(vertexShader, GL_COMPILE_STATUS, &success);
    if (!success)
    {
        glGetShaderInfoLog(vertexShader, 512, NULL, infoLog);
        fprintf(stderr, "ERROR::SHADER::VERTEX::COMPILATION_FAILED\n%s\n", infoLog);
    }

	// Fragement shader
	unsigned int fragmentShader;
	fragmentShader = glCreateShader(GL_FRAGMENT_SHADER);
	glShaderSource(fragmentShader, 1, &fragmentShaderSource, NULL);
	glCompileShader(fragmentShader);
	glGetShaderiv(fragmentShader, GL_COMPILE_STATUS, &success);
    if (!success) {
        glGetShaderInfoLog(fragmentShader, 512, NULL, infoLog);
        fprintf(stderr, "ERROR::SHADER::FRAGMENT::COMPILATION_FAILED\n%s\n", infoLog);
    }


	// Shader program
	unsigned int shaderProgram;
	shaderProgram = glCreateProgram();
	glAttachShader(shaderProgram, vertexShader);
	glAttachShader(shaderProgram, fragmentShader);
	glLinkProgram(shaderProgram);
    glGetProgramiv(shaderProgram, GL_LINK_STATUS, &success);
    if (!success) {
        glGetProgramInfoLog(shaderProgram, 512, NULL, infoLog);
		fprintf(stderr, "ERROR::SHADER::PROGRAM::LINKING_FAILED\n%s\n", infoLog);
    }
    glDeleteShader(vertexShader);
    glDeleteShader(fragmentShader);

	// Pin the uTex sampler to texture unit 0. Has to happen with the program
	// active, and only needs to be done once after linking.
	glUseProgram(shaderProgram);
	glUniform1i(glGetUniformLocation(shaderProgram, "uTex"), 0);


	PFNEGLCREATEIMAGEKHRPROC eglCreateImageKHR = (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
	PFNEGLDESTROYIMAGEKHRPROC eglDestroyImageKHR = (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR"); 
	PFNGLEGLIMAGETARGETTEXTURE2DOESPROC glEGLImageTargetTexture2DOES = (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");
	if (!eglCreateImageKHR || !eglDestroyImageKHR || !glEGLImageTargetTexture2DOES) {
		fprintf(stderr, "EGL extension functions unavailable\n");
		exit(EXIT_FAILURE);
	}

	EGLint img_attribs[] = {
		EGL_WIDTH,                          (EGLint)meta.width,
		EGL_HEIGHT,                         (EGLint)meta.height,
		EGL_LINUX_DRM_FOURCC_EXT,           (EGLint)meta.format,
		EGL_DMA_BUF_PLANE0_FD_EXT,          dmabuf_fd,
		EGL_DMA_BUF_PLANE0_OFFSET_EXT,      0,
		EGL_DMA_BUF_PLANE0_PITCH_EXT,       (EGLint)meta.stride,
		EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT, (EGLint)(meta.modifier & 0xffffffff),
		EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT, (EGLint)(meta.modifier >> 32),
		EGL_NONE,
	};
	EGLDisplay egl_dpy = eglGetCurrentDisplay();
	EGLImageKHR egl_img = eglCreateImageKHR(
		egl_dpy,
		EGL_NO_CONTEXT,
		EGL_LINUX_DMA_BUF_EXT,
		(EGLClientBuffer)NULL,
		img_attribs
	);
	if (egl_img == EGL_NO_IMAGE_KHR) {
		fprintf(stderr, "consumer: eglCreateImageKHR failed: 0x%x\n", eglGetError());
		exit(EXIT_FAILURE);
	}

	unsigned int texture;
	glGenTextures(1, &texture);
	glBindTexture(GL_TEXTURE_2D, texture);
	glEGLImageTargetTexture2DOES(GL_TEXTURE_2D, (GLeglImageOES)egl_img);

	glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
	glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
	glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
	glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE); 
	

	puts("Consumer: imported dmabuf as GL Texture");

	close(dmabuf_fd);

	float vertices[] = {
		0.5f,  0.5f, 0.0f,  1.0f, 1.0f,  // top right
		0.5f, -0.5f, 0.0f,  1.0f, 0.0f,  // bottom right
		-0.5f, -0.5f, 0.0f, 0.0f, 0.0f,  // bottom left
		-0.5f,  0.5f, 0.0f,  0.0f, 1.0f,   // top left 
	};
	unsigned int indices[] = {  // note that we start from 0!
		0, 1, 3,   // first triangle
		1, 2, 3    // second triangle
	};

	unsigned int EBO, VAO, VBO;

	glGenVertexArrays(1, &VAO);
	glGenBuffers(1, &EBO);
	glGenBuffers(1, &VBO);

	glBindVertexArray(VAO);

	glBindBuffer(GL_ELEMENT_ARRAY_BUFFER, EBO);
	glBufferData(GL_ELEMENT_ARRAY_BUFFER, sizeof(indices), indices, GL_STATIC_DRAW);
	glBindBuffer(GL_ARRAY_BUFFER, VBO);
	glBufferData(GL_ARRAY_BUFFER, sizeof(vertices), vertices, GL_STATIC_DRAW);

	glVertexAttribPointer(0, 3, GL_FLOAT, GL_FALSE, 5 * sizeof(float), (void *)0);
	glEnableVertexAttribArray(0);
	glVertexAttribPointer(1, 2, GL_FLOAT, GL_FALSE, 5 * sizeof(float), (void *)(3 * sizeof(float)));
	glEnableVertexAttribArray(1);

	glBindBuffer(GL_ARRAY_BUFFER, 0);
	glBindVertexArray(0);


	// Render loop: clear to blue, present, pump events, repeat. Exit when
	// the user closes the window (via WM, Esc isn't wired up).
	while (!glfwWindowShouldClose(window)) {
		glClearColor(0.2f, 0.4f, 0.8f, 1.0f);
		glClear(GL_COLOR_BUFFER_BIT);

		// Bind the texture to unit 0 each frame. Redundant for this single-
		// texture POC but documents intent and is the shape step 8 needs.
		glActiveTexture(GL_TEXTURE0);
		glBindTexture(GL_TEXTURE_2D, texture);

		glUseProgram(shaderProgram);
		glBindVertexArray(VAO);
		glDrawElements(GL_TRIANGLES, 6, GL_UNSIGNED_INT, 0);
		// glDrawArrays(GL_TRIANGLES, 0, 3);

		// Swap back buffer to the screen. Blocks until the next vsync by
		// default, so the loop runs at the monitor's refresh rate.
		glfwSwapBuffers(window);
		// Proczss pending window-system events (close button, resize, etc.).
		glfwPollEvents();
	}

	glfwDestroyWindow(window);
	glfwTerminate();

	return 0;
}
