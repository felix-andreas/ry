/*
 * Uses the graphics headers' function-like macros the way a device does, so the strict compile
 * in test_headers.rs checks their expansions as well as their definitions. It is only compiled,
 * never linked or run.
 */
#include <Rinternals.h>
#include <R_ext/GraphicsEngine.h>
#include <R_ext/GetX11Image.h>

static int suspended_depth(void) {
    int depth = 0;
    BEGIN_SUSPEND_INTERRUPTS {
        depth = R_interrupts_suspended ? 1 : 0;
    }
    END_SUSPEND_INTERRUPTS;
    return depth;
}

static unsigned int blend(unsigned int color) {
    unsigned int red = R_RED(color), green = R_GREEN(color), blue = R_BLUE(color);
    if (R_TRANSPARENT(color))
        return R_TRANWHITE;
    if (R_OPAQUE(color))
        return R_RGB(blue, green, red);
    return R_RGBA(red, green, blue, R_ALPHA(color) / 2);
}

static void fill_device(pDevDesc device) {
    device->startlty = LTY_DOTDASH;
    device->deviceVersion = R_GE_version;
    device->haveTransparency = 2;
    device->startcol = (int) blend(R_RGB(1, 2, 3));
}

int main(void) {
    DevDesc device;
    fill_device(&device);
    return suspended_depth() + (int) mbcslocale + curDevice() + (int) R_GetX11Image(0, NULL, NULL, NULL);
}
