// One instanced quad per drawn thing: a solid (optionally rounded) rectangle,
// a rounded outline, a tinted glyph mask, or an untinted color glyph.

struct Globals {
    screen: vec2<f32>,
    atlas: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(0) @binding(1) var atlas_tex: texture_2d<f32>;
@group(0) @binding(2) var atlas_smp: sampler;

struct Instance {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv0: vec2<f32>,
    @location(3) uv1: vec2<f32>,
    @location(4) color: vec4<f32>,
    @location(5) kind: u32,
    @location(6) radius: f32,
    @location(7) thickness: f32,
};

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
    @location(2) @interpolate(flat) kind: u32,
    @location(3) local: vec2<f32>,
    @location(4) @interpolate(flat) size: vec2<f32>,
    @location(5) @interpolate(flat) radius: f32,
    @location(6) @interpolate(flat) thickness: f32,
};

@vertex
fn vs_main(@builtin(vertex_index) vid: u32, inst: Instance) -> VsOut {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    let c = corners[vid];
    let px = inst.pos + c * inst.size;
    let ndc = vec2<f32>(px.x / globals.screen.x * 2.0 - 1.0, 1.0 - px.y / globals.screen.y * 2.0);

    var out: VsOut;
    out.clip = vec4<f32>(ndc, 0.0, 1.0);
    out.uv = (inst.uv0 + c * (inst.uv1 - inst.uv0)) / globals.atlas;
    out.color = inst.color;
    out.kind = inst.kind;
    out.local = c * inst.size;
    out.size = inst.size;
    out.radius = inst.radius;
    out.thickness = inst.thickness;
    return out;
}

// Signed distance from p to a rounded box of half-size b, radius r (centered).
fn sd_round_box(p: vec2<f32>, b: vec2<f32>, r: f32) -> f32 {
    let q = abs(p) - b + vec2<f32>(r, r);
    return length(max(q, vec2<f32>(0.0, 0.0))) + min(max(q.x, q.y), 0.0) - r;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    if (in.kind == 4u) {
        // Pac-Man: a disc with a wedge cut out. thickness = mouth half-angle,
        // uv.x (unnormalised, see vs) carries the facing angle.
        let half = in.size * 0.5;
        let p = in.local - half;
        let r = min(half.x, half.y);
        let d = length(p) - r;
        var a = 1.0 - smoothstep(-0.8, 0.4, d);
        let facing = in.uv.x * globals.atlas.x; // undo the atlas normalisation
        let ang = atan2(-p.y, p.x);
        var diff = abs(ang - facing);
        if (diff > 3.14159265) { diff = 6.2831853 - diff; }
        if (diff < in.thickness) { a = 0.0; }
        return vec4<f32>(in.color.rgb, in.color.a * a);
    }
    if (in.kind == 0u || in.kind == 3u) {
        let half = in.size * 0.5;
        let r = min(in.radius, min(half.x, half.y));
        let d = sd_round_box(in.local - half, half, r);
        var a = 1.0 - smoothstep(-0.7, 0.3, d);
        if (in.kind == 3u) {
            // Outline: keep only the band within `thickness` of the edge.
            let inner = 1.0 - smoothstep(-0.7, 0.3, d + in.thickness);
            a = a - inner;
        }
        return vec4<f32>(in.color.rgb, in.color.a * a);
    }
    let t = textureSample(atlas_tex, atlas_smp, in.uv);
    if (in.kind == 1u) {
        return vec4<f32>(in.color.rgb, in.color.a * t.a);
    }
    return vec4<f32>(t.rgb, t.a * in.color.a);
}
