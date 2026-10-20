// The Trigo Group mark: four curved bands, traced from the trigo-group.com
// favicon (512 px grid, kept as the viewBox so the geometry stays readable).
const BAND_TOPS = [120, 207.33, 294.67, 382];
const BAND_HEIGHT = 65.5;
// Each band rises 56 px from left to right along a quadratic curve.
const RISE = 56;

const bandPath = (top: number) => {
  const bottom = top + BAND_HEIGHT;
  return `M160 ${top}Q256 ${top} 352 ${top - RISE}V${bottom - RISE}Q256 ${bottom} 160 ${bottom}Z`;
};

const TrigoMark = ({
  width,
  height,
  className = "",
}: {
  width?: number | string;
  height?: number | string;
  className?: string;
}) => (
  <svg
    width={width ?? 12}
    height={height ?? 24}
    viewBox="160 64 192 384"
    className={`fill-brand-green ${className}`}
    xmlns="http://www.w3.org/2000/svg"
    aria-hidden="true"
  >
    {BAND_TOPS.map((top) => (
      <path key={top} d={bandPath(top)} />
    ))}
  </svg>
);

export default TrigoMark;
