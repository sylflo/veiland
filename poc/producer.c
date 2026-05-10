
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
#include <GLES2/gl2ext.h>
#include <stdint.h>


struct buffer_msg {
	uint32_t width;
	uint32_t height;
	uint32_t format;
	uint32_t stride;
	uint64_t modifier;
};

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

	const int BUF_W = 800, BUF_H = 600;
	struct gbm_bo *bo = gbm_bo_create(
		gbm,
		BUF_W, BUF_H,
		GBM_FORMAT_ARGB8888,
		GBM_BO_USE_RENDERING | GBM_BO_USE_LINEAR
	);
	if (!bo) {
		fprintf(stderr, "gbm_bo_create failed\n");
		exit(EXIT_FAILURE);
	}

	printf("producer: allocated GBM buffer %dx%d, format ARGB8888, stride=%u, modifier=0x%lx\n",
		BUF_W, BUF_H,
		gbm_bo_get_stride(bo),
		(unsigned long)gbm_bo_get_modifier(bo)
	);

	PFNEGLCREATEIMAGEKHRPROC eglCreateImageKHR = (PFNEGLCREATEIMAGEKHRPROC)eglGetProcAddress("eglCreateImageKHR");
	PFNEGLDESTROYIMAGEKHRPROC eglDestroyImageKHR = (PFNEGLDESTROYIMAGEKHRPROC)eglGetProcAddress("eglDestroyImageKHR"); 
	PFNGLEGLIMAGETARGETTEXTURE2DOESPROC glEGLImageTargetTexture2DOES = (PFNGLEGLIMAGETARGETTEXTURE2DOESPROC)eglGetProcAddress("glEGLImageTargetTexture2DOES");

	if (!eglCreateImageKHR || !eglDestroyImageKHR || !glEGLImageTargetTexture2DOES) {
		fprintf(stderr, "EGL extension functions unavailable\n");
		exit(EXIT_FAILURE);
	}

	EGLImageKHR egl_img = eglCreateImageKHR(
		egl_dpy,
		EGL_NO_CONTEXT,
		EGL_NATIVE_PIXMAP_KHR,
		(EGLClientBuffer)bo,
		NULL
	);
	if (egl_img == EGL_NO_IMAGE_KHR) {
		fprintf(stderr, "eglCreateImageKHR failed: 0x%x\n", eglGetError());
		exit(EXIT_FAILURE);
	}

	GLuint render_tex;
	glGenTextures(1, &render_tex);
	glBindTexture(GL_TEXTURE_2D, render_tex);
	glEGLImageTargetTexture2DOES(GL_TEXTURE_2D, (GLeglImageOES)egl_img);

	GLuint fbo;
	glGenFramebuffers(1, &fbo);
	glBindFramebuffer(GL_FRAMEBUFFER, fbo);
	glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, render_tex, 0);
	GLenum status = glCheckFramebufferStatus(GL_FRAMEBUFFER); 
	if (status != GL_FRAMEBUFFER_COMPLETE) { 
		fprintf(stderr, "FBO incomplete: 0x%x\n", status);
		exit(EXIT_FAILURE);
	}
	printf("producer: FBO ready, render target %dx%d\n", BUF_W, BUF_H);

	glViewport(0, 0, BUF_W, BUF_H);

	const int N_FRAMES = 60;
	for (int frame = 0; frame < N_FRAMES; frame++) {
		float t = (float)frame / N_FRAMES;
		glClearColor(t, 0.5f, 1.0f -t, 1.0f);
		glClear(GL_COLOR_BUFFER_BIT);
	}
	GLenum err = glGetError();
	if (err != GL_NO_ERROR) {
		fprintf(stderr, "GL error during render: 0x%x\n", err);
	}
	unsigned char rgba[4];
	glReadPixels(0, 0, 1, 1, GL_RGBA, GL_UNSIGNED_BYTE, rgba);
	printf("producer: pixel(0,0) = (%u, %u, %u, %u)\n", rgba[0], rgba[1], rgba[2], rgba[3]);

	glFinish();

	printf("Producer: rendered %d frames into the buffer\n", N_FRAMES);

	int dmabuf_fd = gbm_bo_get_fd(bo);
	if (dmabuf_fd == -1) {
		perror("gbm_bo_get_fd");
		exit(EXIT_FAILURE);
	}

	struct buffer_msg meta = {
		.width = BUF_W,
		.height = BUF_H,
		.format = GBM_FORMAT_ARGB8888,
		.stride = gbm_bo_get_stride(bo),
		.modifier = gbm_bo_get_modifier(bo),
	};

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

	if (connect(sock_fd, (struct sockaddr *)&addr, sizeof(addr)) == -1) {
        perror("connect failed");
        exit(EXIT_FAILURE);
	}
	printf("Connected\n");

	struct iovec iov = { .iov_base = &meta, .iov_len = sizeof(meta) };

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

	memcpy(CMSG_DATA(cmsg), &dmabuf_fd, sizeof(int));

	if (sendmsg(sock_fd, &msg, 0) == -1) {
		perror("sendmsg");
		exit(EXIT_FAILURE);
	}

	printf("producer: sent fd=%d, %ux%u, format=0x%x, stride=%u, modifier=0x%lx\n",
		dmabuf_fd, meta.width, meta.height, meta.format, meta.stride,
		(unsigned long)meta.modifier);

	sleep(60);

	glDeleteFramebuffers(1, &fbo);
	glDeleteTextures(1, &render_tex);
	eglDestroyImageKHR(egl_dpy, egl_img);
	gbm_bo_destroy(bo);
	eglMakeCurrent(egl_dpy, EGL_NO_SURFACE, EGL_NO_SURFACE, EGL_NO_CONTEXT);
	eglDestroyContext(egl_dpy, egl_ctx);
	eglTerminate(egl_dpy);
	gbm_device_destroy(gbm);
	close(drm_fd);
	close(dmabuf_fd);
	close(sock_fd);

	return 0;
}
