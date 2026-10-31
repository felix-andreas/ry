/*
 * ry's declaration of R's X11 image capture hook, source-compatible with R 4.6. ry has no X11
 * device, so R_GetX11Image always returns FALSE.
 */
#ifndef GETX11IMAGE_H_
#define GETX11IMAGE_H_

#include <R_ext/Boolean.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Copies the image of X11 device d (0-based) into *pximage, an XImage**, and its size into
   *pwidth and *pheight, returning FALSE when the device is not an X11 device. */
Rboolean R_GetX11Image(int d, void *pximage, int *pwidth, int *pheight);

#ifdef __cplusplus
}
#endif

#endif
