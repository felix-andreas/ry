/*
 * ry's declaration of R's graphics engine API, source- and layout-compatible with R 4.6.
 *
 * Devices such as ragg and svglite fill in these structures and pass them to the engine, so the
 * struct layouts, enum values and constants are a binary interface and match GNU R's exactly.
 * Like GNU R's header, this one expects Rinternals.h (for SEXP and cetype_t) to be included
 * first, and it includes R_ext/GraphicsDevice.h itself.
 */
#ifndef R_GRAPHICSENGINE_H_
#define R_GRAPHICSENGINE_H_

#ifdef __cplusplus
extern "C" {
#endif

/*
 * Engine versions. A device records the version it was compiled against in
 * DevDesc.deviceVersion, and the engine only calls the entry points that version has.
 */
#define R_GE_definitions 13
#define R_GE_deviceClip 14
#define R_GE_group 15
#define R_GE_glyphs 16
#define R_GE_fontVar 17
#define R_GE_version R_GE_fontVar

int R_GE_getVersion(void);
void R_GE_checkVersionOrDie(int version);

typedef enum {
    GE_DEVICE = 0,
    GE_NDC = 1,
    GE_INCHES = 2,
    GE_CM = 3
} GEUnit;

#define MAX_GRAPHICS_SYSTEMS 24

typedef enum {
    GE_InitState = 0,
    GE_FinaliseState = 1,
    GE_SaveState = 2,
    GE_CopyState = 3,
    GE_SaveSnapshotState = 4,
    GE_RestoreSnapshotState = 5,
    GE_RestoreState = 6,
    GE_CheckPlot = 7,
    GE_ScalePS = 8
} GEevent;

typedef enum {
    GE_ROUND_CAP = 1,
    GE_BUTT_CAP = 2,
    GE_SQUARE_CAP = 3
} R_GE_lineend;

typedef enum {
    GE_ROUND_JOIN = 1,
    GE_MITRE_JOIN = 2,
    GE_BEVEL_JOIN = 3
} R_GE_linejoin;

/* The drawing state the engine hands to every device call. Colours are packed rcolor values. */
typedef struct {
    int col;
    int fill;
    double gamma;
    double lwd;
    int lty;
    R_GE_lineend lend;
    R_GE_linejoin ljoin;
    double lmitre;
    double cex;
    double ps;
    double lineheight;
    int fontface;
    char fontfamily[201];
    SEXP patternFill;
} R_GE_gcontext;

typedef R_GE_gcontext *pGEcontext;

#ifdef __cplusplus
}
#endif

#include <R_ext/GraphicsDevice.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct _GEDevDesc GEDevDesc;

typedef SEXP (*GEcallback)(GEevent, GEDevDesc *, SEXP);

/* A graphics system (base graphics, grid) registered with a device. */
typedef struct {
    void *systemSpecific;
    GEcallback callback;
} GESystemDesc;

/* The engine's view of a device: the device itself plus its display list and system state. */
struct _GEDevDesc {
    pDevDesc dev;
    Rboolean displayListOn;
    SEXP displayList;
    SEXP DLlastElt;
    SEXP savedSnapshot;
    Rboolean dirty;
    Rboolean recordGraphics;
    /* GNU R 4.3 has no lock field, so the fields from here on sit at different offsets in
       devices compiled against it. */
    Rboolean lock;
    GESystemDesc *gesd[MAX_GRAPHICS_SYSTEMS];
    Rboolean ask;
    Rboolean appending;
};

typedef GEDevDesc *pGEDevDesc;

#define desc2GEDesc Rf_desc2GEDesc
pGEDevDesc desc2GEDesc(pDevDesc dd);
int GEdeviceNumber(pGEDevDesc dd);
pGEDevDesc GEgetDevice(int number);
void GEaddDevice(pGEDevDesc dd);
void GEaddDevice2(pGEDevDesc dd, const char *name);
void GEaddDevice2f(pGEDevDesc dd, const char *name, const char *file);
void GEkillDevice(pGEDevDesc dd);
pDevDesc GEcreateDD(void);
void GEfreeDD(pDevDesc dd);
pGEDevDesc GEcreateDevDesc(pDevDesc dev);
void GEdestroyDevDesc(pGEDevDesc dd);
void *GEsystemState(pGEDevDesc dd, int index);
void GEregisterWithDevice(pGEDevDesc dd);
void GEregisterSystem(GEcallback callback, int *systemRegisterIndex);
void GEunregisterSystem(int registerIndex);
SEXP GEhandleEvent(GEevent event, pDevDesc dev, SEXP data);

#define fromDeviceX GEfromDeviceX
#define toDeviceX GEtoDeviceX
#define fromDeviceY GEfromDeviceY
#define toDeviceY GEtoDeviceY
#define fromDeviceWidth GEfromDeviceWidth
#define toDeviceWidth GEtoDeviceWidth
#define fromDeviceHeight GEfromDeviceHeight
#define toDeviceHeight GEtoDeviceHeight

double fromDeviceX(double value, GEUnit to, pGEDevDesc dd);
double toDeviceX(double value, GEUnit from, pGEDevDesc dd);
double fromDeviceY(double value, GEUnit to, pGEDevDesc dd);
double toDeviceY(double value, GEUnit from, pGEDevDesc dd);
double fromDeviceWidth(double value, GEUnit to, pGEDevDesc dd);
double toDeviceWidth(double value, GEUnit from, pGEDevDesc dd);
double fromDeviceHeight(double value, GEUnit to, pGEDevDesc dd);
double toDeviceHeight(double value, GEUnit from, pGEDevDesc dd);

/* A colour packed as red in the low byte, then green, blue and alpha. */
typedef unsigned int rcolor;

#define RGBpar Rf_RGBpar
#define RGBpar3 Rf_RGBpar3
#define col2name Rf_col2name

rcolor RGBpar(SEXP value, int index);
rcolor RGBpar3(SEXP value, int index, rcolor background);
const char *col2name(rcolor col);
rcolor R_GE_str2col(const char *s);

/* Line types pack dash and gap lengths into successive hexadecimal digits. */
#define LTY_BLANK -1
#define LTY_SOLID 0
#define LTY_DASHED 4 + (4 << 4)
#define LTY_DOTTED 1 + (3 << 4)
#define LTY_DOTDASH 1 + (3 << 4) + (4 << 8) + (3 << 12)
#define LTY_LONGDASH 7 + (3 << 4)
#define LTY_TWODASH 2 + (2 << 4) + (6 << 8) + (2 << 12)

R_GE_lineend GE_LENDpar(SEXP value, int ind);
SEXP GE_LENDget(R_GE_lineend lend);
R_GE_linejoin GE_LJOINpar(SEXP value, int ind);
SEXP GE_LJOINget(R_GE_linejoin ljoin);

void GESetClip(double x1, double y1, double x2, double y2, pGEDevDesc dd);
void GENewPage(const pGEcontext gc, pGEDevDesc dd);
void GELine(double x1, double y1, double x2, double y2, const pGEcontext gc, pGEDevDesc dd);
void GEPolyline(int n, double *x, double *y, const pGEcontext gc, pGEDevDesc dd);
void GEPolygon(int n, double *x, double *y, const pGEcontext gc, pGEDevDesc dd);
SEXP GEXspline(int n, double *x, double *y, double *s, Rboolean open, Rboolean repEnds,
               Rboolean draw, const pGEcontext gc, pGEDevDesc dd);
void GECircle(double x, double y, double radius, const pGEcontext gc, pGEDevDesc dd);
void GERect(double x0, double y0, double x1, double y1, const pGEcontext gc, pGEDevDesc dd);
void GEPath(double *x, double *y, int npoly, int *nper, Rboolean winding, const pGEcontext gc,
            pGEDevDesc dd);
void GERaster(unsigned int *raster, int w, int h, double x, double y, double width,
              double height, double angle, Rboolean interpolate, const pGEcontext gc,
              pGEDevDesc dd);
SEXP GECap(pGEDevDesc dd);
void GEText(double x, double y, const char *const str, cetype_t enc, double xc, double yc,
            double rot, const pGEcontext gc, pGEDevDesc dd);
void GEMode(int mode, pGEDevDesc dd);
void GESymbol(double x, double y, int pch, double size, const pGEcontext gc, pGEDevDesc dd);
void GEPretty(double *lo, double *up, int *ndiv);
void GEMetricInfo(int c, const pGEcontext gc, double *ascent, double *descent, double *width,
                  pGEDevDesc dd);
double GEStrWidth(const char *str, cetype_t enc, const pGEcontext gc, pGEDevDesc dd);
double GEStrHeight(const char *str, cetype_t enc, const pGEcontext gc, pGEDevDesc dd);
void GEStrMetric(const char *str, cetype_t enc, const pGEcontext gc, double *ascent,
                 double *descent, double *width, pGEDevDesc dd);
int GEstring_to_pch(SEXP pch);

unsigned int GE_LTYpar(SEXP value, int index);
SEXP GE_LTYget(unsigned int lty);

void R_GE_rasterScale(unsigned int *sraster, int sw, int sh, unsigned int *draster, int dw,
                      int dh);
void R_GE_rasterInterpolate(unsigned int *sraster, int sw, int sh, unsigned int *draster, int dw,
                            int dh);
void R_GE_rasterRotatedSize(int w, int h, double angle, int *wnew, int *hnew);
void R_GE_rasterRotatedOffset(int w, int h, double angle, int botleft, double *xoff,
                              double *yoff);
void R_GE_rasterResizeForRotation(unsigned int *sraster, int w, int h, unsigned int *newRaster,
                                  int wnew, int hnew, const pGEcontext gc);
void R_GE_rasterRotate(unsigned int *sraster, int w, int h, double angle,
                       unsigned int *draster, const pGEcontext gc, Rboolean perPixelAlpha);

double GEExpressionWidth(SEXP expr, const pGEcontext gc, pGEDevDesc dd);
double GEExpressionHeight(SEXP expr, const pGEcontext gc, pGEDevDesc dd);
void GEExpressionMetric(SEXP expr, const pGEcontext gc, double *ascent, double *descent,
                        double *width, pGEDevDesc dd);
void GEMathText(double x, double y, SEXP expr, double xc, double yc, double rot,
                const pGEcontext gc, pGEDevDesc dd);

SEXP GEcontourLines(double *x, int nx, double *y, int ny, double *z, double *levels, int nl);

#define DEG2RAD 0.01745329251994329576

pGEDevDesc GEcurrentDevice(void);
Rboolean GEdeviceDirty(pGEDevDesc dd);
void GEdirtyDevice(pGEDevDesc dd);
Rboolean GEcheckState(pGEDevDesc dd);
Rboolean GErecording(SEXP call, pGEDevDesc dd);
void GErecordGraphicOperation(SEXP op, SEXP args, pGEDevDesc dd);
void GEinitDisplayList(pGEDevDesc dd);
void GEplayDisplayList(pGEDevDesc dd);
void GEcopyDisplayList(int fromDevice);
SEXP GEcreateSnapshot(pGEDevDesc dd);
void GEplaySnapshot(SEXP snapshot, pGEDevDesc dd);
void GEonExit(void);

#define CreateAtVector Rf_CreateAtVector
SEXP CreateAtVector(double axp[], const double usr[], int nint, Rboolean logflag);
#define GAxisPars Rf_GAxisPars
void GAxisPars(double *min, double *max, int *n, Rboolean log, int axis);

SEXP Rf_eval_with_gd(SEXP expr, SEXP rho, pGEDevDesc dd);
#define eval_with_gd Rf_eval_with_gd

/* Fill patterns: gradients and tilings a device reads through these accessors. */
Rboolean R_GE_isPattern(SEXP x);

#define R_GE_linearGradientPattern 1
#define R_GE_radialGradientPattern 2
#define R_GE_tilingPattern 3

int R_GE_patternType(SEXP pattern);

double R_GE_linearGradientX1(SEXP pattern);
double R_GE_linearGradientY1(SEXP pattern);
double R_GE_linearGradientX2(SEXP pattern);
double R_GE_linearGradientY2(SEXP pattern);
int R_GE_linearGradientNumStops(SEXP pattern);
double R_GE_linearGradientStop(SEXP pattern, int i);
rcolor R_GE_linearGradientColour(SEXP pattern, int i);

#define R_GE_patternExtendPad 1
#define R_GE_patternExtendRepeat 2
#define R_GE_patternExtendReflect 3
#define R_GE_patternExtendNone 4

int R_GE_linearGradientExtend(SEXP pattern);

double R_GE_radialGradientCX1(SEXP pattern);
double R_GE_radialGradientCY1(SEXP pattern);
double R_GE_radialGradientR1(SEXP pattern);
double R_GE_radialGradientCX2(SEXP pattern);
double R_GE_radialGradientCY2(SEXP pattern);
double R_GE_radialGradientR2(SEXP pattern);
int R_GE_radialGradientNumStops(SEXP pattern);
double R_GE_radialGradientStop(SEXP pattern, int i);
rcolor R_GE_radialGradientColour(SEXP pattern, int i);
int R_GE_radialGradientExtend(SEXP pattern);

SEXP R_GE_tilingPatternFunction(SEXP pattern);
double R_GE_tilingPatternX(SEXP pattern);
double R_GE_tilingPatternY(SEXP pattern);
double R_GE_tilingPatternWidth(SEXP pattern);
double R_GE_tilingPatternHeight(SEXP pattern);
int R_GE_tilingPatternExtend(SEXP pattern);

/* Compositing operators for groups: the Porter-Duff operators, then the blend modes. */
#define R_GE_compositeClear 1
#define R_GE_compositeSource 2
#define R_GE_compositeOver 3
#define R_GE_compositeIn 4
#define R_GE_compositeOut 5
#define R_GE_compositeAtop 6
#define R_GE_compositeDest 7
#define R_GE_compositeDestOver 8
#define R_GE_compositeDestIn 9
#define R_GE_compositeDestOut 10
#define R_GE_compositeDestAtop 11
#define R_GE_compositeXor 12
#define R_GE_compositeAdd 13
#define R_GE_compositeSaturate 14
#define R_GE_compositeMultiply 15
#define R_GE_compositeScreen 16
#define R_GE_compositeOverlay 17
#define R_GE_compositeDarken 18
#define R_GE_compositeLighten 19
#define R_GE_compositeColorDodge 20
#define R_GE_compositeColorBurn 21
#define R_GE_compositeHardLight 22
#define R_GE_compositeSoftLight 23
#define R_GE_compositeDifference 24
#define R_GE_compositeExclusion 25

#define R_GE_nonZeroWindingRule 1
#define R_GE_evenOddRule 2

int R_GE_clipPathFillRule(SEXP path);

void GEStroke(SEXP path, const pGEcontext gc, pGEDevDesc dd);
void GEFill(SEXP path, int rule, const pGEcontext gc, pGEDevDesc dd);
void GEFillStroke(SEXP path, int rule, const pGEcontext gc, pGEDevDesc dd);

#define R_GE_alphaMask 1
#define R_GE_luminanceMask 2

int R_GE_maskType(SEXP mask);

/* Indices into the capabilities vector a device's capabilities() callback fills in. */
#define R_GE_capability_semiTransparency 0
#define R_GE_capability_transparentBackground 1
#define R_GE_capability_rasterImage 2
#define R_GE_capability_capture 3
#define R_GE_capability_locator 4
#define R_GE_capability_events 5
#define R_GE_capability_patterns 6
#define R_GE_capability_clippingPaths 7
#define R_GE_capability_masks 8
#define R_GE_capability_compositing 9
#define R_GE_capability_transformations 10
#define R_GE_capability_paths 11
#define R_GE_capability_glyphs 12
#define R_GE_capability_variableFonts 13

#define R_GE_text_style_normal 1
#define R_GE_text_style_italic 2
#define R_GE_text_style_oblique 3

/* Rendered glyph runs: the glyphs, their positions and fonts, as the glyph() callback sees them. */
SEXP R_GE_glyphInfoGlyphs(SEXP glyphInfo);
SEXP R_GE_glyphInfoFonts(SEXP glyphInfo);
SEXP R_GE_glyphID(SEXP glyphs);
SEXP R_GE_glyphX(SEXP glyphs);
SEXP R_GE_glyphY(SEXP glyphs);
SEXP R_GE_glyphFont(SEXP glyphs);
SEXP R_GE_glyphSize(SEXP glyphs);
SEXP R_GE_glyphColour(SEXP glyphs);
SEXP R_GE_glyphRotation(SEXP glyphs);
Rboolean R_GE_hasGlyphRotation(SEXP glyphs);
const char *R_GE_glyphFontFile(SEXP glyphFont);
int R_GE_glyphFontIndex(SEXP glyphFont);
const char *R_GE_glyphFontFamily(SEXP glyphFont);
double R_GE_glyphFontWeight(SEXP glyphFont);
int R_GE_glyphFontStyle(SEXP glyphFont);
const char *R_GE_glyphFontPSname(SEXP glyphFont);
int R_GE_glyphFontNumVar(SEXP glyphFont);
const char *R_GE_glyphFontVarAxis(SEXP glyphFont, int index);
double R_GE_glyphFontVarValue(SEXP glyphFont, int index);
const char *R_GE_glyphFontVarFormatted(SEXP glyphFont, int index);

void GEGlyph(int n, int *glyphs, double *x, double *y, SEXP font, double size, int colour,
             double rot, pGEDevDesc dd);

#ifdef __cplusplus
}
#endif

#endif
