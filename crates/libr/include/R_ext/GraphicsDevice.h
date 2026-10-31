/*
 * ry's declaration of R's graphics device interface, source- and layout-compatible with R 4.6.
 *
 * A device allocates a DevDesc, fills in its fields and callbacks, and hands it to the engine.
 * Devices compiled against GNU R write this struct directly, so every field keeps GNU R's type
 * and position. Include R_ext/GraphicsEngine.h, which includes this header after defining the
 * types the callbacks take.
 */
#ifndef R_GRAPHICSDEVICE_H_
#define R_GRAPHICSDEVICE_H_

/*
 * A device that passes a pointer to its own structure where DevDesc* is expected can define
 * R_USE_PROTOTYPES to 0 before including this header, which declares the callbacks without
 * parameter lists.
 */
#ifndef R_USE_PROTOTYPES
#define R_USE_PROTOTYPES 1
#ifndef R_GRAPHICSENGINE_H_
#error R_ext/GraphicsEngine.h must be included first, and includes this header
#endif
#endif

#ifdef __cplusplus
#include <cstddef>
#else
#include <stddef.h>
#endif

#include <R_ext/libextern.h>
#include <R_ext/Boolean.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct _DevDesc DevDesc;
typedef DevDesc *pDevDesc;

#if R_USE_PROTOTYPES
#define RY_DEVICE_CALLBACK(result, name, parameters) result(*name) parameters
#else
#define RY_DEVICE_CALLBACK(result, name, parameters) result(*name)()
#endif

struct _DevDesc {
    /* The device extent in device units, and the current clipping rectangle. */
    double left;
    double right;
    double bottom;
    double top;
    double clipLeft;
    double clipRight;
    double clipBottom;
    double clipTop;

    double xCharOffset;
    double yCharOffset;
    double yLineBias;
    /* Inches per device unit and character size in device units, as [x, y]. */
    double ipr[2];
    double cra[2];
    double gamma;

    Rboolean canClip;
    Rboolean canChangeGamma;
    /* Horizontal text adjustment: 0 none, 1 only {0, 0.5, 1}, 2 anything in [0, 1]. */
    int canHAdj;

    /* Initial graphical parameters. */
    double startps;
    int startcol;
    int startfill;
    int startlty;
    int startfont;
    double startgamma;

    void *deviceSpecific;

    Rboolean displayListOn;

    Rboolean canGenMouseDown;
    Rboolean canGenMouseMove;
    Rboolean canGenMouseUp;
    Rboolean canGenKeybd;
    Rboolean canGenIdle;
    Rboolean gettingEvent;

    RY_DEVICE_CALLBACK(void, activate, (const pDevDesc));
    RY_DEVICE_CALLBACK(void, circle, (double x, double y, double r, const pGEcontext gc,
                                      pDevDesc dd));
    RY_DEVICE_CALLBACK(void, clip, (double x0, double x1, double y0, double y1, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, close, (pDevDesc dd));
    RY_DEVICE_CALLBACK(void, deactivate, (pDevDesc));
    RY_DEVICE_CALLBACK(Rboolean, locator, (double *x, double *y, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, line, (double x1, double y1, double x2, double y2,
                                    const pGEcontext gc, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, metricInfo, (int c, const pGEcontext gc, double *ascent,
                                          double *descent, double *width, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, mode, (int mode, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, newPage, (const pGEcontext gc, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, polygon, (int n, double *x, double *y, const pGEcontext gc,
                                       pDevDesc dd));
    RY_DEVICE_CALLBACK(void, polyline, (int n, double *x, double *y, const pGEcontext gc,
                                        pDevDesc dd));
    RY_DEVICE_CALLBACK(void, rect, (double x0, double y0, double x1, double y1,
                                    const pGEcontext gc, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, path, (double *x, double *y, int npoly, int *nper,
                                    Rboolean winding, const pGEcontext gc, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, raster, (unsigned int *raster, int w, int h, double x, double y,
                                      double width, double height, double rot,
                                      Rboolean interpolate, const pGEcontext gc, pDevDesc dd));
    RY_DEVICE_CALLBACK(SEXP, cap, (pDevDesc dd));
    RY_DEVICE_CALLBACK(void, size, (double *left, double *right, double *bottom, double *top,
                                    pDevDesc dd));
    RY_DEVICE_CALLBACK(double, strWidth, (const char *str, const pGEcontext gc, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, text, (double x, double y, const char *str, double rot,
                                    double hadj, const pGEcontext gc, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, onExit, (pDevDesc dd));

    /* No longer called by the engine; the slot stays so later fields keep their offsets. */
    SEXP (*getEvent)(SEXP, const char *);

    RY_DEVICE_CALLBACK(Rboolean, newFrameConfirm, (pDevDesc dd));

    /* When set, text outside the symbol font arrives in UTF-8 at textUTF8 and strWidthUTF8,
       and metricInfo takes a negative c as a Unicode code point. */
    Rboolean hasTextUTF8;
    RY_DEVICE_CALLBACK(void, textUTF8, (double x, double y, const char *str, double rot,
                                        double hadj, const pGEcontext gc, pDevDesc dd));
    RY_DEVICE_CALLBACK(double, strWidthUTF8, (const char *str, const pGEcontext gc,
                                              pDevDesc dd));
    Rboolean wantSymbolUTF8;

    Rboolean useRotatedTextInContour;

    SEXP eventEnv;
    RY_DEVICE_CALLBACK(void, eventHelper, (pDevDesc dd, int code));

    RY_DEVICE_CALLBACK(int, holdflush, (pDevDesc dd, int level));

    /* Capabilities, where 0 means unset: 1 is no, and 2 or more name the level of support. */
    int haveTransparency;
    int haveTransparentBg;
    int haveRaster;
    int haveCapture, haveLocator;

    /* Since engine version R_GE_definitions. */
    RY_DEVICE_CALLBACK(SEXP, setPattern, (SEXP pattern, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, releasePattern, (SEXP ref, pDevDesc dd));
    RY_DEVICE_CALLBACK(SEXP, setClipPath, (SEXP path, SEXP ref, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, releaseClipPath, (SEXP ref, pDevDesc dd));
    RY_DEVICE_CALLBACK(SEXP, setMask, (SEXP path, SEXP ref, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, releaseMask, (SEXP ref, pDevDesc dd));
    int deviceVersion;

    /* Since engine version R_GE_deviceClip. */
    Rboolean deviceClip;

    /* Since engine version R_GE_group. */
    RY_DEVICE_CALLBACK(SEXP, defineGroup, (SEXP source, int op, SEXP destination,
                                           pDevDesc dd));
    RY_DEVICE_CALLBACK(void, useGroup, (SEXP ref, SEXP trans, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, releaseGroup, (SEXP ref, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, stroke, (SEXP path, const pGEcontext gc, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, fill, (SEXP path, int rule, const pGEcontext gc, pDevDesc dd));
    RY_DEVICE_CALLBACK(void, fillStroke, (SEXP path, int rule, const pGEcontext gc,
                                          pDevDesc dd));
    RY_DEVICE_CALLBACK(SEXP, capabilities, (SEXP cap));

    /* Since engine version R_GE_glyphs. */
    RY_DEVICE_CALLBACK(void, glyph, (int n, int *glyphs, double *x, double *y, SEXP font,
                                     double size, int colour, double rot, pDevDesc dd));

    /* Zeroed space for fields later engine versions add, so a device compiled now keeps
       working when loaded into a later R. */
    char reserved[64];
};

#undef RY_DEVICE_CALLBACK

#define R_RGB(r, g, b) ((r) | ((g) << 8) | ((b) << 16) | 0xFF000000)
#define R_RGBA(r, g, b, a) ((r) | ((g) << 8) | ((b) << 16) | ((a) << 24))
#define R_RED(col) (((col)) & 255)
#define R_GREEN(col) (((col) >> 8) & 255)
#define R_BLUE(col) (((col) >> 16) & 255)
#define R_ALPHA(col) (((col) >> 24) & 255)
#define R_OPAQUE(col) (R_ALPHA(col) == 255)
#define R_TRANSPARENT(col) (R_ALPHA(col) == 0)
#define R_TRANWHITE (R_RGBA(255, 255, 255, 0))

#define curDevice Rf_curDevice
#define killDevice Rf_killDevice
#define ndevNumber Rf_ndevNumber
#define NewFrameConfirm Rf_NewFrameConfirm
#define nextDevice Rf_nextDevice
#define NoDevices Rf_NoDevices
#define NumDevices Rf_NumDevices
#define prevDevice Rf_prevDevice
#define selectDevice Rf_selectDevice
#define AdobeSymbol2utf8 Rf_AdobeSymbol2utf8
#define utf8toAdobeSymbol Rf_utf8toAdobeSymbol
#define utf8Toutf8NoPUA Rf_utf8Toutf8NoPUA
#define utf8ToLatin1AdobeSymbol2utf8 Rf_utf8ToLatin1AdobeSymbol2utf8

int ndevNumber(pDevDesc dd);
int NumDevices(void);
void R_CheckDeviceAvailable(void);
Rboolean R_CheckDeviceAvailableBool(void);
int curDevice(void);
int nextDevice(int from);
int prevDevice(int from);
int selectDevice(int devNum);
void killDevice(int devNum);
int NoDevices(void);
void NewFrameConfirm(pDevDesc dd);

typedef enum {
    knUNKNOWN = -1,
    knLEFT = 0,
    knUP,
    knRIGHT,
    knDOWN,
    knF1,
    knF2,
    knF3,
    knF4,
    knF5,
    knF6,
    knF7,
    knF8,
    knF9,
    knF10,
    knF11,
    knF12,
    knPGUP,
    knPGDN,
    knEND,
    knHOME,
    knINS,
    knDEL
} R_KeyName;

typedef enum {
    meMouseDown = 0,
    meMouseUp,
    meMouseMove
} R_MouseEvent;

/* Mouse button bits for doMouseEvent's buttons argument. */
#define leftButton 1
#define middleButton 2
#define rightButton 4

#define doKeybd Rf_doKeybd
#define doMouseEvent Rf_doMouseEvent
#define doIdle Rf_doIdle
#define doesIdle Rf_doesIdle

void doMouseEvent(pDevDesc dd, R_MouseEvent event, int buttons, double x, double y);
void doKeybd(pDevDesc dd, R_KeyName rkey, const char *keyname);
void doIdle(pDevDesc dd);
Rboolean doesIdle(pDevDesc dd);

/*
 * A device brackets work that must not be interrupted midway (a half-drawn frame, a window
 * system call) with these. An interrupt that arrives meanwhile stays pending and is taken by
 * END_SUSPEND_INTERRUPTS, unless an enclosing pair still suspends interrupts.
 */
#ifndef BEGIN_SUSPEND_INTERRUPTS
#define BEGIN_SUSPEND_INTERRUPTS                                                                  \
    do {                                                                                          \
        Rboolean __oldsusp__ = R_interrupts_suspended;                                            \
        R_interrupts_suspended = TRUE;
#define END_SUSPEND_INTERRUPTS                                                                    \
    R_interrupts_suspended = __oldsusp__;                                                         \
    if (R_interrupts_pending && !R_interrupts_suspended)                                          \
        Rf_onintr();                                                                              \
    }                                                                                             \
    while (0)

LibExtern Rboolean R_interrupts_suspended;
LibExtern int R_interrupts_pending;
extern void Rf_onintr(void);
LibExtern Rboolean mbcslocale;
#endif

extern void *AdobeSymbol2utf8(char *out, const char *in, size_t nwork, Rboolean usePUA);
extern int utf8toAdobeSymbol(char *out, const char *in);
const char *utf8Toutf8NoPUA(const char *in);
const char *utf8ToLatin1AdobeSymbol2utf8(const char *in, Rboolean usePUA);

/* Writes the UTF-8 encoding of code point c and a terminating NUL into s, which must hold at
   least 7 bytes, and returns the number of bytes written before the NUL. */
extern size_t Rf_ucstoutf8(char *s, const unsigned int c);

#ifdef __cplusplus
}
#endif

#endif
