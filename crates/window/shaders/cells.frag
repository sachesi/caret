#version 450
#extension GL_GOOGLE_include_directive : require
#include "common.glsl"

layout(set = 0, binding = 0) uniform sampler2D atlas;

layout(location = 0) flat in uint cell_index;
layout(location = 1) in vec2 local;
layout(location = 2) in vec2 texel;

layout(location = 0) out vec4 color;

const uint UNDERLINE = 1u;
const uint DOUBLE_UNDERLINE = 2u;
const uint UNDERCURL = 4u;
const uint DOTTED_UNDERLINE = 8u;
const uint DASHED_UNDERLINE = 16u;
const uint STRIKEOUT = 32u;

bool within(float y, int top, int thickness) {
    return y >= float(top) && y < float(top + thickness);
}

// Coverage of the lines under or through the cell at `p`, in its pixels.
float lines(Cell cell, vec2 p) {
    uint flags = cell.flags;
    float x = p.x + float(int(cell_index % k.columns) * k.cell_width);
    float t = float(k.underline_thickness);
    if ((flags & UNDERLINE) != 0u && within(p.y, k.underline_top, k.underline_thickness)) {
        return 1.0;
    }
    if ((flags & DOUBLE_UNDERLINE) != 0u) {
        int below = min(k.underline_top + 2 * k.underline_thickness, k.cell_height - k.underline_thickness);
        if (within(p.y, k.underline_top, k.underline_thickness) || within(p.y, below, k.underline_thickness)) {
            return 1.0;
        }
    }
    if ((flags & DOTTED_UNDERLINE) != 0u && within(p.y, k.underline_top, k.underline_thickness)) {
        return mod(floor(x / t), 2.0) == 0.0 ? 1.0 : 0.0;
    }
    if ((flags & DASHED_UNDERLINE) != 0u && within(p.y, k.underline_top, k.underline_thickness)) {
        return mod(floor(x / (3.0 * t)), 2.0) == 0.0 ? 1.0 : 0.0;
    }
    if ((flags & UNDERCURL) != 0u) {
        float period = max(float(k.cell_width), 4.0 * t);
        float amplitude = t;
        float centre = float(k.underline_top) + amplitude;
        float wave = centre + amplitude * sin(x / period * 6.2831853);
        return clamp(t * 0.75 - abs(p.y + 0.5 - wave) + 0.5, 0.0, 1.0);
    }
    if ((flags & STRIKEOUT) != 0u && within(p.y, k.strikeout_top, k.strikeout_thickness)) {
        return 1.0;
    }
    return 0.0;
}

// Coverage of the cursor's outline, bar or line at `p`, for shapes other than a block.
float cursor_mark(vec2 p) {
    uint shape = k.cursor_shape & 15u;
    bool focused = (k.cursor_shape & 32u) != 0u;
    float width = float(k.cell_width) * ((k.cursor_shape & 16u) != 0u ? 2.0 : 1.0);
    float x = p.x + float((int(cell_index % k.columns) - k.cursor_column) * k.cell_width);
    float thickness = max(1.0, float(k.underline_thickness));
    if (shape == 0u && !focused || shape == 3u) {
        bool edge = x < thickness || x >= width - thickness
            || p.y < thickness || p.y >= float(k.cell_height) - thickness;
        return edge ? 1.0 : 0.0;
    }
    if (shape == 1u) {
        return p.y >= float(k.cell_height) - 2.0 * thickness ? 1.0 : 0.0;
    }
    if (shape == 2u) {
        return x < 2.0 * thickness ? 1.0 : 0.0;
    }
    return 0.0;
}

void main() {
    Cell cell = cells[cell_index];
    if (k.pass == 0u) {
        vec3 background = (cell.flags & HAS_BACKGROUND) != 0u ? rgb(cell.bg) : rgb(k.background);
        if ((cell.flags & SELECTED) != 0u) {
            background = rgb(k.selection);
        }
        if (block_cursor(cell_index)) {
            background = rgb(k.cursor_color);
        }
        background = mix(background, rgb(cell.underline), lines(cell, local));
        if (under_cursor(cell_index)) {
            background = mix(background, rgb(k.cursor_color), cursor_mark(local));
        }
        color = vec4(background, 1.0);
    } else {
        Glyph glyph = glyphs[cell.glyph - 1u];
        vec4 sampled = texelFetch(atlas, ivec2(texel), 0);
        if ((glyph.flags & COLOR_GLYPH) != 0u) {
            color = sampled;
        } else {
            vec3 foreground = block_cursor(cell_index) ? rgb(k.background) : rgb(cell.fg);
            color = vec4(foreground * sampled.a, sampled.a);
        }
    }
}
