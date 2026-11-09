// One fullscreen pass: cell texture (glyph, level, tint, glow) + glyph atlas -> EDR (extended linear sRGB) or sRGB.
// A line-for-line port of glyph.hlsl; compiled at startup with newLibraryWithSource (no Xcode step).
// All per-frame logic runs on the CPU at cell resolution; this shader only turns cells into pixels.

#include <metal_stdlib>
using namespace metal;

struct Consts
{
    float2 grid_origin;   // pixel position of cell (0,0)'s top-left corner
    float2 cell;          // cell size in pixels (integers)
    uint2  grid_dim;      // columns, rows
    uint   atlas_cols;    // tiles per atlas row
    uint   hdr;           // 1: float EDR target (1.0 = SDR white), 0: 8-bit sRGB target
    float4 bands[6];      // linear colours: 0 = background, 1..5 = dim .. bright
    float4 amber;         // linear amber, for the warm tint
    float4 glow_col;      // linear glow colour
    float  white;         // SDR white in target units (1 on macOS: EDR is SDR-white relative)
    float  peak;          // highlight cap in target units (capped by the screen's EDR headroom)
    float  highlight;     // 0..1 highlight intensity
    float  glow_gain;
    float  bright;        // glyphs at rest (OLED care rest scale x idle dim x APL limiter)
    float  fx_bright;     // glyphs in an effect (level > 5) and glow (idle dim x APL limiter)
    float  bg_dim;        // background (idle dim)
    float  pad0;
};

struct VsOut { float4 pos [[position]]; };

vertex VsOut vs_main(uint id [[vertex_id]])
{
    VsOut o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}

static float3 band_colour(constant Consts& c, float L)
{
    float l = clamp(L, 0.0, 5.0);
    int i = min((int)floor(l), 4);
    float f = l - i;
    return mix(c.bands[i].rgb, c.bands[i + 1].rgb, f);
}

static float luma(float3 c) { return dot(c, float3(0.2126, 0.7152, 0.0722)); }

static float3 to_srgb(float3 c)
{
    c = saturate(c);
    return select(1.055 * pow(c, 1.0 / 2.4) - 0.055, c * 12.92, c <= 0.0031308);
}

static float glow_at(constant Consts& c, texture2d<float> cells, int2 p)
{
    if (p.x < 0 || p.y < 0 || p.x >= (int)c.grid_dim.x || p.y >= (int)c.grid_dim.y) return 0;
    return cells.read(uint2(p)).w;
}

fragment float4 ps_main(VsOut i [[stage_in]], constant Consts& c [[buffer(0)]],
                        texture2d<float> cells [[texture(0)]], texture2d<float> atlas [[texture(1)]])
{
    float2 p = i.pos.xy - c.grid_origin;
    float2 gc = p / c.cell;
    int2 ci = (int2)floor(gc);
    float3 col = c.bands[0].rgb * c.bg_dim;

    if (ci.x >= 0 && ci.y >= 0 && ci.x < (int)c.grid_dim.x && ci.y < (int)c.grid_dim.y)
    {
        float4 d = cells.read(uint2(ci));
        uint g = (uint)(d.x + 0.5);
        if (g > 0)
        {
            int2 local = (int2)(p - float2(ci) * c.cell);
            int2 tile = int2(g % c.atlas_cols, g / c.atlas_cols) * (int2)c.cell;
            float a = atlas.read(uint2(tile + local)).r;
            float3 gcol = band_colour(c, d.y);
            if (d.z > 0)
            {
                float3 warm = c.amber.rgb * (luma(gcol) / max(luma(c.amber.rgb), 1e-4));
                gcol = mix(gcol, warm, saturate(d.z));
            }
            float o = max(d.y - 5.0, 0.0);
            float m = 1.0;
            if (c.hdr == 0)
            {
                gcol = mix(gcol, float3(1, 1, 1), saturate(o * 0.5) * 0.6);
            }
            else
            {
                // levels above 5 lift the glyph's luminance from its band colour toward the peak, scaled by the
                // highlight setting; everything at or below level 5 stays at or below SDR white
                float bl = max(luma(gcol), 1e-4);
                float k = saturate(o / 2.0);
                k = k * (2.0 - k);
                float tl = mix(bl, max(bl, c.peak / c.white), k * c.highlight);
                m = tl / bl;
            }
            float b = mix(c.bright, c.fx_bright, saturate(o));
            col = mix(col, gcol * m * b, a);
        }
    }

    // glow: bilinear over the cell grid's glow channel (blurred on the CPU)
    float2 gp = gc - 0.5;
    int2 g0 = (int2)floor(gp);
    float2 f = gp - float2(g0);
    float gl = mix(mix(glow_at(c, cells, g0), glow_at(c, cells, g0 + int2(1, 0)), f.x),
                   mix(glow_at(c, cells, g0 + int2(0, 1)), glow_at(c, cells, g0 + int2(1, 1)), f.x), f.y) * c.fx_bright;
    col += c.glow_col.rgb * gl * c.glow_gain;

    if (c.hdr == 0)
        return float4(to_srgb(col), 1);

    // EDR: extended linear sRGB, SDR white at `white`, highlights capped at `peak`
    float3 outc = col * c.white
        + c.glow_col.rgb * gl * c.glow_gain * c.white * c.highlight * max(c.peak / c.white - 1.0, 0.0) * 0.5;
    float mx = max(outc.r, max(outc.g, outc.b));
    if (mx > c.peak) outc *= c.peak / mx;
    return float4(outc, 1);
}
