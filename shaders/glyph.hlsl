// One fullscreen pass: cell texture (glyph, level, tint, glow) + glyph atlas -> scRGB (HDR) or sRGB (SDR).
// All per-frame logic (object, field, effects, glow blur) runs on the CPU at cell resolution; this shader only
// turns cells into pixels.

cbuffer Consts : register(b0)
{
    float2 grid_origin;   // pixel position of cell (0,0)'s top-left corner
    float2 cell;          // cell size in pixels (integers)
    uint2  grid_dim;      // columns, rows
    uint   atlas_cols;    // tiles per atlas row
    uint   hdr;           // 1: scRGB FP16 target, 0: 8-bit sRGB target
    float4 bands[6];      // linear colours: 0 = background, 1..5 = dim .. bright
    float4 amber;         // linear amber, for the warm tint
    float4 glow_col;      // linear glow colour
    float  white;         // SDR white in scRGB units (nits / 80); 1 in SDR mode
    float  peak;          // highlight cap in scRGB units
    float  highlight;     // 0..1 highlight intensity
    float  glow_gain;
    float  bright;        // glyphs at rest (OLED care rest scale x idle dim x APL limiter)
    float  fx_bright;     // glyphs in an effect (level > 5) and glow (idle dim x APL limiter)
    float  bg_dim;        // background (idle dim)
    float  pad0;
};

Texture2D<float4> cells : register(t0);
Texture2D<float>  atlas : register(t1);

struct VsOut { float4 pos : SV_Position; };

VsOut vs_main(uint id : SV_VertexID)
{
    VsOut o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
    return o;
}

float3 band_colour(float L)
{
    float l = clamp(L, 0.0, 5.0);
    int i = min((int)floor(l), 4);
    float f = l - i;
    return lerp(bands[i].rgb, bands[i + 1].rgb, f);
}

float luma(float3 c) { return dot(c, float3(0.2126, 0.7152, 0.0722)); }

float3 to_srgb(float3 c)
{
    c = saturate(c);
    return c <= 0.0031308 ? c * 12.92 : 1.055 * pow(c, 1.0 / 2.4) - 0.055;
}

float glow_at(int2 c)
{
    if (c.x < 0 || c.y < 0 || c.x >= (int)grid_dim.x || c.y >= (int)grid_dim.y) return 0;
    return cells.Load(int3(c, 0)).w;
}

float4 ps_main(VsOut i) : SV_Target
{
    float2 p = i.pos.xy - grid_origin;
    float2 gc = p / cell;
    int2 c = (int2)floor(gc);
    float3 col = bands[0].rgb * bg_dim;

    if (c.x >= 0 && c.y >= 0 && c.x < (int)grid_dim.x && c.y < (int)grid_dim.y)
    {
        float4 d = cells.Load(int3(c, 0));
        uint g = (uint)(d.x + 0.5);
        if (g > 0)
        {
            int2 local = (int2)(p - c * cell);
            int2 tile = int2(g % atlas_cols, g / atlas_cols) * (int2)cell;
            float a = atlas.Load(int3(tile + local, 0));
            float3 gcol = band_colour(d.y);
            if (d.z > 0)
            {
                float3 warm = amber.rgb * (luma(gcol) / max(luma(amber.rgb), 1e-4));
                gcol = lerp(gcol, warm, saturate(d.z));
            }
            float o = max(d.y - 5.0, 0.0);
            float m = 1.0;
            if (hdr == 0)
            {
                gcol = lerp(gcol, float3(1, 1, 1), saturate(o * 0.5) * 0.6);
            }
            else
            {
                // levels above 5 lift the glyph's luminance from its band colour toward the peak, scaled by the
                // highlight setting; everything at or below level 5 stays at or below SDR white
                float bl = max(luma(gcol), 1e-4);
                float k = saturate(o / 2.0);
                k = k * (2.0 - k);
                float tl = lerp(bl, max(bl, peak / white), k * highlight);
                m = tl / bl;
            }
            float b = lerp(bright, fx_bright, saturate(o));
            col = lerp(col, gcol * m * b, a);
        }
    }

    // glow: bilinear over the cell grid's glow channel (blurred on the CPU)
    float2 gp = gc - 0.5;
    int2 g0 = (int2)floor(gp);
    float2 f = gp - g0;
    float gl = lerp(lerp(glow_at(g0), glow_at(g0 + int2(1, 0)), f.x),
                    lerp(glow_at(g0 + int2(0, 1)), glow_at(g0 + int2(1, 1)), f.x), f.y) * fx_bright;
    col += glow_col.rgb * gl * glow_gain;

    if (hdr == 0)
        return float4(to_srgb(col), 1);

    // HDR: scRGB, 1.0 = 80 nits; SDR white sits at `white`, highlights are capped at `peak`
    float3 outc = col * white + glow_col.rgb * gl * glow_gain * white * highlight * max(peak / white - 1.0, 0.0) * 0.5;
    float mx = max(outc.r, max(outc.g, outc.b));
    if (mx > peak) outc *= peak / mx;
    return float4(outc, 1);
}
