/*
 * Prints the size and offset of every field of the graphics structures that devices fill in
 * and the engine reads, one "struct.field offset size" line each. Compiled against ry's headers
 * and against GNU R's, the outputs must agree, because a device compiled against either one
 * shares these structures with the engine across the shared library boundary.
 *
 * GEDevDesc gained its lock field after R 4.3; define PROBE_LOCK when the headers have it.
 */
#include <stddef.h>
#include <stdio.h>
#include <Rinternals.h>
#include <R_ext/GraphicsEngine.h>

#define FIELD(type, field)                                                                         \
    printf("%s.%s %zu %zu\n", #type, #field, offsetof(type, field), sizeof(((type *) 0)->field))
#define SIZE(type) printf("%s %zu\n", #type, sizeof(type))

int main(void) {
    SIZE(DevDesc);
    FIELD(DevDesc, left);
    FIELD(DevDesc, right);
    FIELD(DevDesc, bottom);
    FIELD(DevDesc, top);
    FIELD(DevDesc, clipLeft);
    FIELD(DevDesc, clipRight);
    FIELD(DevDesc, clipBottom);
    FIELD(DevDesc, clipTop);
    FIELD(DevDesc, xCharOffset);
    FIELD(DevDesc, yCharOffset);
    FIELD(DevDesc, yLineBias);
    FIELD(DevDesc, ipr);
    FIELD(DevDesc, cra);
    FIELD(DevDesc, gamma);
    FIELD(DevDesc, canClip);
    FIELD(DevDesc, canChangeGamma);
    FIELD(DevDesc, canHAdj);
    FIELD(DevDesc, startps);
    FIELD(DevDesc, startcol);
    FIELD(DevDesc, startfill);
    FIELD(DevDesc, startlty);
    FIELD(DevDesc, startfont);
    FIELD(DevDesc, startgamma);
    FIELD(DevDesc, deviceSpecific);
    FIELD(DevDesc, displayListOn);
    FIELD(DevDesc, canGenMouseDown);
    FIELD(DevDesc, canGenMouseMove);
    FIELD(DevDesc, canGenMouseUp);
    FIELD(DevDesc, canGenKeybd);
    FIELD(DevDesc, canGenIdle);
    FIELD(DevDesc, gettingEvent);
    FIELD(DevDesc, activate);
    FIELD(DevDesc, circle);
    FIELD(DevDesc, clip);
    FIELD(DevDesc, close);
    FIELD(DevDesc, deactivate);
    FIELD(DevDesc, locator);
    FIELD(DevDesc, line);
    FIELD(DevDesc, metricInfo);
    FIELD(DevDesc, mode);
    FIELD(DevDesc, newPage);
    FIELD(DevDesc, polygon);
    FIELD(DevDesc, polyline);
    FIELD(DevDesc, rect);
    FIELD(DevDesc, path);
    FIELD(DevDesc, raster);
    FIELD(DevDesc, cap);
    FIELD(DevDesc, size);
    FIELD(DevDesc, strWidth);
    FIELD(DevDesc, text);
    FIELD(DevDesc, onExit);
    FIELD(DevDesc, getEvent);
    FIELD(DevDesc, newFrameConfirm);
    FIELD(DevDesc, hasTextUTF8);
    FIELD(DevDesc, textUTF8);
    FIELD(DevDesc, strWidthUTF8);
    FIELD(DevDesc, wantSymbolUTF8);
    FIELD(DevDesc, useRotatedTextInContour);
    FIELD(DevDesc, eventEnv);
    FIELD(DevDesc, eventHelper);
    FIELD(DevDesc, holdflush);
    FIELD(DevDesc, haveTransparency);
    FIELD(DevDesc, haveTransparentBg);
    FIELD(DevDesc, haveRaster);
    FIELD(DevDesc, haveCapture);
    FIELD(DevDesc, haveLocator);
    FIELD(DevDesc, setPattern);
    FIELD(DevDesc, releasePattern);
    FIELD(DevDesc, setClipPath);
    FIELD(DevDesc, releaseClipPath);
    FIELD(DevDesc, setMask);
    FIELD(DevDesc, releaseMask);
    FIELD(DevDesc, deviceVersion);
    FIELD(DevDesc, deviceClip);
    FIELD(DevDesc, defineGroup);
    FIELD(DevDesc, useGroup);
    FIELD(DevDesc, releaseGroup);
    FIELD(DevDesc, stroke);
    FIELD(DevDesc, fill);
    FIELD(DevDesc, fillStroke);
    FIELD(DevDesc, capabilities);
    FIELD(DevDesc, glyph);
    FIELD(DevDesc, reserved);

    SIZE(R_GE_gcontext);
    FIELD(R_GE_gcontext, col);
    FIELD(R_GE_gcontext, fill);
    FIELD(R_GE_gcontext, gamma);
    FIELD(R_GE_gcontext, lwd);
    FIELD(R_GE_gcontext, lty);
    FIELD(R_GE_gcontext, lend);
    FIELD(R_GE_gcontext, ljoin);
    FIELD(R_GE_gcontext, lmitre);
    FIELD(R_GE_gcontext, cex);
    FIELD(R_GE_gcontext, ps);
    FIELD(R_GE_gcontext, lineheight);
    FIELD(R_GE_gcontext, fontface);
    FIELD(R_GE_gcontext, fontfamily);
    FIELD(R_GE_gcontext, patternFill);

    SIZE(GESystemDesc);
    FIELD(GESystemDesc, systemSpecific);
    FIELD(GESystemDesc, callback);

    SIZE(GEDevDesc);
    FIELD(GEDevDesc, dev);
    FIELD(GEDevDesc, displayListOn);
    FIELD(GEDevDesc, displayList);
    FIELD(GEDevDesc, DLlastElt);
    FIELD(GEDevDesc, savedSnapshot);
    FIELD(GEDevDesc, dirty);
    FIELD(GEDevDesc, recordGraphics);
#ifdef PROBE_LOCK
    FIELD(GEDevDesc, lock);
#endif
    FIELD(GEDevDesc, gesd);
    FIELD(GEDevDesc, ask);
    FIELD(GEDevDesc, appending);

    printf("R_GE_version %d\n", (int) R_GE_version);
    printf("MAX_GRAPHICS_SYSTEMS %d\n", (int) MAX_GRAPHICS_SYSTEMS);
    printf("GE_ScalePS %d\n", (int) GE_ScalePS);
    printf("GE_RestoreState %d\n", (int) GE_RestoreState);
    printf("GE_SQUARE_CAP %d\n", (int) GE_SQUARE_CAP);
    printf("GE_BEVEL_JOIN %d\n", (int) GE_BEVEL_JOIN);
    printf("GE_CM %d\n", (int) GE_CM);
    printf("knDEL %d\n", (int) knDEL);
    printf("meMouseMove %d\n", (int) meMouseMove);
    printf("LTY_DOTDASH %d\n", (int) (LTY_DOTDASH));
    printf("LTY_TWODASH %d\n", (int) (LTY_TWODASH));
    printf("R_TRANWHITE %u\n", (unsigned int) R_TRANWHITE);
    printf("R_RGB %u\n", (unsigned int) R_RGB(1, 2, 3));
    printf("R_GE_compositeExclusion %d\n", R_GE_compositeExclusion);
    printf("R_GE_capability_glyphs %d\n", R_GE_capability_glyphs);
    return 0;
}
