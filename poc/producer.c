
// SPDX-License-Identifier: GPL-3.0-or-later

#include <stdlib.h>
#include <unistd.h>
#include <stdio.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <string.h>
#include <sys/types.h>
#include <fcntl.h>
#include <errno.h>
#include <gbm.h>
#include <EGL/egl.h>
#include <EGL/eglext.h>
#include <GLES3/gl3.h>

int main(void) {
	puts("producer: hello");

	int drm_fd = open("/dev/dri/renderD128", O_RDWR);
	if (drm_fd == -1) {
		perror("open /dev/dri/renderD128");
		exit(EXIT_FAILURE);
	}

	struct gbm_device *gbm = gbm_create_device(drm_fd);
	if (!gbm) {
		fprintf(stderr, "gbm_create_device_failed\n");
		close(drm_fd);
		exit(EXIT_FAILURE);
	}
	printf("producer: opened DRM fd %d, got GBM devices %p\n", drm_fd, (void *)gbm);

	EGLDisplay egl_dpy = eglGetPlatformDisplay(EGL_PLATFORM_GBM_KHR, gbm, NULL);
	if (egl_dpy == EGL_NO_DISPLAY) {                                                                                                                                                                                                                                                                     
		fprintf(stderr, "eglGetPlatformDisplay failed: 0x%x\n", eglGetError());
		exit(EXIT_FAILURE);                                                                                                                                                                                                                                                                              
	}

	EGLint major, minor;
	if (!eglInitialize(egl_dpy, &major, &minor)) {
		fprintf(stderr, "eglInitialize failed: 0x%x\n", eglGetError());                                                                                                                                                                                                                                  
		exit(EXIT_FAILURE);                                                                                                                                                                                                                                                                              
	}
	printf("producer: EGL %d.%d (%s)\n", major, minor, eglQueryString(egl_dpy, EGL_VERSION));

	if (!eglBindAPI(EGL_OPENGL_ES_API)) {
		fprintf(stderr, "eglBindAPI(GLES) failed: 0%x\n", eglGetError());
		exit(EXIT_FAILURE);
	}

	EGLint config_attribs[] = {
		EGL_SURFACE_TYPE, EGL_WINDOW_BIT,
		EGL_RENDERABLE_TYPE, EGL_OPENGL_ES3_BIT,
		EGL_RED_SIZE,        8,
		EGL_GREEN_SIZE,        8, 
		EGL_BLUE_SIZE,        8,
		EGL_ALPHA_SIZE,        8, 
		EGL_NONE,

	};
	EGLConfig egl_cfg;
	EGLint num_configs;
	if (!eglChooseConfig(egl_dpy, config_attribs, &egl_cfg, 1, &num_configs) || num_configs < 1) {
		fprintf(stderr, "eglChooseConfig failed: 0x%x\n", eglGetError());
		exit(EXIT_FAILURE);
	}

	EGLint context_attribs[] = {
		EGL_CONTEXT_MAJOR_VERSION, 3,
		EGL_CONTEXT_MINOR_VERSION, 0,
		EGL_NONE,
	};

	EGLContext egl_ctx = eglCreateContext(egl_dpy, egl_cfg, EGL_NO_CONTEXT, context_attribs);
	if (egl_ctx == EGL_NO_CONTEXT) {
		fprintf(stderr, "eglCreateContext failed: 0x%x\n", eglGetError());
		exit(EXIT_FAILURE);
	}

	if (!eglMakeCurrent(egl_dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, egl_ctx)) {
		fprintf(stderr, "eglMakeCurrent failed: 0x%x\n", eglGetError());
		exit(EXIT_FAILURE);
	}

	printf("producer: GL %s\n", (const char *)glGetString(GL_VERSION));
	printf("producer: GLSL %s\n", (const char *)glGetString(GL_SHADING_LANGUAGE_VERSION));
	printf("producer: vendor: %s\n", (const char *)glGetString(GL_VENDOR));
	printf("producer: renderer: %s\n", (const char *)glGetString(GL_RENDERER));

	// to move later when server socket will be back
	eglMakeCurrent(egl_dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
	eglDestroyContext(egl_dpy, egl_ctx);
	eglTerminate(egl_dpy);
	gbm_device_destroy(gbm);
	close(drm_fd);


	// // Create socket
	// struct sockaddr_un addr;
	// int sock_fd = socket(AF_UNIX, SOCK_STREAM, 0);
	
    // if (sock_fd == -1) {
    //     perror("socket failed");
    //     exit(EXIT_FAILURE);
    // }
	// memset(&addr, 0, sizeof(addr));
	// addr.sun_family = AF_UNIX;
	// strncpy(addr.sun_path, "/tmp/veiland-poc.sock", sizeof(addr.sun_path) - 1);

	// if (connect(sock_fd, (struct sockaddr *)&addr, sizeof addr) == -1) {
    //     perror("connect failed");
    //     exit(EXIT_FAILURE);
	// }
	// printf("Connected\n");

	// int file_fd = open("/tmp/scm-test.txt", O_RDONLY);
	// if (file_fd == -1) {
	// 	perror("open");
	// 	exit(EXIT_FAILURE);
	// }

	// char dummy = 'X';
	// struct iovec iov = { .iov_base = &dummy, .iov_len = 1 };

	// char cmsg_buf[CMSG_SPACE(sizeof(int))];
	// memset(cmsg_buf, 0, sizeof(cmsg_buf));


	// struct msghdr msg = {0};
	// msg.msg_iov = &iov;
	// msg.msg_iovlen = 1;
	// msg.msg_control = cmsg_buf;
	// msg.msg_controllen = sizeof(cmsg_buf);

	// struct cmsghdr *cmsg = CMSG_FIRSTHDR(&msg);
	// cmsg->cmsg_level = SOL_SOCKET;
	// cmsg->cmsg_type = SCM_RIGHTS;
	// cmsg->cmsg_len = CMSG_LEN(sizeof(int));

	// memcpy(CMSG_DATA(cmsg), &file_fd, sizeof(int));

	// if (sendmsg(sock_fd, &msg, 0) == -1) {
	// 	perror("sendmsg");
	// 	exit(EXIT_FAILURE);
	// }

	// close(file_fd);

	return 0;
}
