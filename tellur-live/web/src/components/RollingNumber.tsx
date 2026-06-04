import { useEffect } from "react";
import {
  motion,
  useReducedMotion,
  useSpring,
  useTransform,
} from "motion/react";

// Slot-machine style numeric readout. A string like "00:05.42" or "150F" is
// split into characters; each DIGIT becomes a vertical column of 0–9 that rolls
// (translateY) so the active digit sits in a 1em-tall window, while non-digits
// (":", ".", "F", spaces, …) render as fixed glyphs. The roll is spring-driven,
// so changing a digit scrolls the column up/down to the new value. Under
// reduced-motion the column jumps with no roll.
//
// The whole control is `1em` tall with `overflow: hidden` per digit window, so
// it drops into any text context and inherits font-size/weight/color. Use
// tabular-nums on the surrounding text so digit columns keep a steady width.

interface RollingDigitProps {
  digit: number;
  reduceMotion: boolean;
}

// One 0–9 column. The spring holds the (fractional, mid-roll) digit value; the
// column is translated up by `value` rows so row `value` lands in the window.
function RollingDigit({ digit, reduceMotion }: RollingDigitProps) {
  const value = useSpring(digit, { stiffness: 320, damping: 32, restDelta: 0.001 });

  useEffect(() => {
    if (reduceMotion) {
      value.jump(digit);
    } else {
      value.set(digit);
    }
  }, [digit, reduceMotion, value]);

  // translateY in em: each row is 1em tall, so row N sits at -N em.
  const transform = useTransform(value, (v) => `translateY(${-v}em)`);

  return (
    <span className="rolling-digit">
      <motion.span className="rolling-digit__column" style={{ transform }}>
        {DIGITS.map((d) => (
          <span className="rolling-digit__cell" key={d}>
            {d}
          </span>
        ))}
      </motion.span>
    </span>
  );
}

const DIGITS = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

interface RollingNumberProps {
  // The fully-formatted string (digits + separators/suffix); digits roll, the
  // rest is shown verbatim.
  value: string;
  className?: string;
}

export function RollingNumber({ value, className }: RollingNumberProps) {
  // `useReducedMotion` can return null before it resolves; treat that as "no
  // reduce" so the first paint still animates once known.
  const reduceMotion = useReducedMotion() ?? false;

  return (
    <span className={className ? `rolling-number ${className}` : "rolling-number"}>
      {value.split("").map((ch, i) =>
        ch >= "0" && ch <= "9" ? (
          <RollingDigit
            // Index-keyed: the formatted string is fixed-width (e.g. MM:SS.FF),
            // so each position is a stable digit slot that rolls in place.
            key={i}
            digit={Number(ch)}
            reduceMotion={reduceMotion}
          />
        ) : (
          <span className="rolling-number__sep" key={i}>
            {ch}
          </span>
        ),
      )}
    </span>
  );
}
