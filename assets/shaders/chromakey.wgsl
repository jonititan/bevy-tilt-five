// Chromakey fragment shader for the Mixcast webcam plate.
// Keys out a specified RGB color with smoothness falloff and simple spill
// suppression. Based on the standard Chroma-key subtract approach used by
// OBS and similar tools.

#import bevy_pbr::pbr_fragment::pbr_input_from_standard_material
#import bevy_pbr::pbr_functions::alpha_discard
#import bevy_pbr::forward_io::{VertexOutput, FragmentOutput}
#import bevy_pbr::pbr_functions as fns

@group(2) @binding(100) var<uniform> key_rgb: vec4<f32>;
@group(2) @binding(101) var<uniform> params: vec4<f32>; // similarity, smoothness, spill, _

fn luma(c: vec3<f32>) -> f32 { return dot(c, vec3<f32>(0.299, 0.587, 0.114)); }

fn chroma_distance(sample: vec3<f32>, key: vec3<f32>) -> f32 {
    // Simple distance in YCbCr-like space (approximation).
    let s_y = luma(sample);
    let k_y = luma(key);
    let s_cb = sample.b - s_y;
    let s_cr = sample.r - s_y;
    let k_cb = key.b - k_y;
    let k_cr = key.r - k_y;
    return sqrt((s_cb - k_cb) * (s_cb - k_cb) + (s_cr - k_cr) * (s_cr - k_cr));
}

@fragment
fn fragment(in: VertexOutput, @builtin(front_facing) is_front: bool) -> FragmentOutput {
    var pbr_input = pbr_input_from_standard_material(in, is_front);
    let base = pbr_input.material.base_color.rgb;

    let d = chroma_distance(base, key_rgb.rgb);
    let similarity = params.x;
    let smoothness = params.y;
    let spill = params.z;

    let alpha = smoothstep(similarity, similarity + smoothness, d);

    // Spill suppression: desaturate toward luma where distance is small.
    let y = luma(base);
    let desat = mix(vec3<f32>(y), base, 1.0 - spill * (1.0 - alpha));

    pbr_input.material.base_color = vec4<f32>(desat, alpha);
    pbr_input = alpha_discard(pbr_input.material, pbr_input);

    var out: FragmentOutput;
    out.color = fns::apply_pbr_lighting(pbr_input);
    return out;
}
