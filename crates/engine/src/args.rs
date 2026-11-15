//! The options of one plate rewrite: postprocess.py's command-line arguments,
//! with the same defaults. A project sets them (project.rs); the CLI parses
//! them from flags.

/// Options for rewriting one plate. Field names follow postprocess.py's.
#[derive(Debug, Clone, PartialEq)]
pub struct RunArgs {
    pub min_area: f64,
    pub min_segment: Option<f64>,
    pub extra_inset: f64,
    pub close: f64,
    pub wall_overlap: f64,
    pub tile_gap: Option<f64>,
    /// Layout SVG: the outlines the pieces slide onto.
    pub svg: Option<String>,
    pub svg_paths: String,
    pub svg_assign: Option<String>,
    pub svg_skip_unmatched: bool,
    pub svg_scale: Option<f64>,
    pub svg_offset: (f64, f64),
    pub svg_default_angle: Option<f64>,
    /// Pattern SVG file.
    pub pattern: Option<String>,
    /// Pattern SVG text, for a pattern made from a reference (no file needed).
    pub pattern_svg: Option<String>,
    pub pattern_label: Option<String>,
    pub pattern_rotate: Option<f64>,
    pub pattern_origin: Option<(f64, f64)>,
    pub pattern_scale: f64,
    pub pattern_grout: Option<f64>,
    pub border: Option<i64>,
    pub edge_rim: Option<i64>,
    pub hole_rim: Option<i64>,
    pub max_z: Option<f64>,
    pub no_merge: bool,
    pub flow: f64,
    pub print_speed: Option<f64>,
    pub travel_speed: f64,
    pub retract: f64,
    pub retract_speed: f64,
    pub retract_threshold: f64,
    pub zhop: f64,
    pub layer_height: f64,
    pub line_width: f64,
    pub min_coverage: f64,
    pub quiet: bool,
}

impl Default for RunArgs {
    fn default() -> Self {
        RunArgs {
            min_area: 20.0,
            min_segment: None,
            extra_inset: 0.0,
            close: 3.0,
            wall_overlap: 0.15,
            tile_gap: None,
            svg: None,
            svg_paths: "direction".into(),
            svg_assign: None,
            svg_skip_unmatched: false,
            svg_scale: None,
            svg_offset: (0.0, 0.0),
            svg_default_angle: None,
            pattern: None,
            pattern_svg: None,
            pattern_label: None,
            pattern_rotate: None,
            pattern_origin: None,
            pattern_scale: 1.0,
            pattern_grout: None,
            border: None,
            edge_rim: None,
            hole_rim: None,
            max_z: None,
            no_merge: false,
            flow: 1.0,
            print_speed: None,
            travel_speed: 300.0,
            retract: 0.8,
            retract_speed: 30.0,
            retract_threshold: 2.0,
            zhop: 0.4,
            layer_height: 0.2,
            line_width: 0.42,
            min_coverage: 0.6,
            quiet: false,
        }
    }
}

impl RunArgs {
    fn has_pattern(&self) -> bool {
        self.pattern.is_some() || self.pattern_svg.is_some()
    }

    /// Fill in defaults that depend on other options (`finish_args`).
    pub fn finish(&mut self) -> Result<(), String> {
        let layout = self.svg.is_some();
        if self.border.is_none() {
            self.border = Some(if layout { 0 } else { 1 });
        }
        let gap = *self.tile_gap.get_or_insert(if layout { 0.0 } else { 0.4 });
        if self.has_pattern() && !layout {
            return Err(
                "--pattern needs --svg LAYOUT (the piece outlines to repeat it over)".into(),
            );
        }
        if gap < 0.0 {
            return Err("--tile-gap must be at least 0".into());
        }
        Ok(())
    }

    /// The options the surface rewrite reads.
    pub fn engine_args(&self) -> crate::engine::Args {
        let loops = |v: Option<i64>| v.map(|n| n.max(0) as usize);
        crate::engine::Args {
            close: self.close,
            extra_inset: self.extra_inset,
            wall_overlap: self.wall_overlap,
            min_area: self.min_area,
            min_coverage: self.min_coverage,
            min_segment: self.min_segment,
            layer_height: Some(self.layer_height),
            flow: self.flow,
            print_speed: self.print_speed,
            travel_speed: self.travel_speed,
            retract: self.retract,
            retract_speed: self.retract_speed,
            retract_threshold: self.retract_threshold,
            zhop: self.zhop,
            max_z: self.max_z,
            no_merge: Some(self.no_merge),
            edge_rim: loops(self.edge_rim),
            hole_rim: loops(self.hole_rim),
            border: loops(self.border),
            grout_depth: None,
            raise: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finish_fills_dependent_defaults() {
        let mut a = RunArgs::default();
        a.finish().unwrap();
        assert_eq!((a.border, a.tile_gap), (Some(1), Some(0.4)));
        let mut a = RunArgs {
            svg: Some("l.svg".into()),
            ..Default::default()
        };
        a.finish().unwrap();
        assert_eq!((a.border, a.tile_gap), (Some(0), Some(0.0)));
        let mut a = RunArgs {
            pattern: Some("p.svg".into()),
            ..Default::default()
        };
        assert!(a.finish().unwrap_err().contains("--svg LAYOUT"));
        let mut a = RunArgs {
            tile_gap: Some(-1.0),
            ..Default::default()
        };
        assert!(a.finish().is_err());
    }
}
