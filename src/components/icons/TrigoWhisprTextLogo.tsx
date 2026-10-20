import TrigoMark from "./TrigoMark";

// Product name, not translated.
const BRAND = "Trigo";
const PRODUCT = "Whispr";

// The Trigo mark followed by the "Trigo Whispr" wordmark. `width` sets the
// overall width; the type and the mark scale with it.
const TrigoWhisprTextLogo = ({
  width = 120,
  className = "",
}: {
  width?: number;
  height?: number;
  className?: string;
}) => {
  const fontSize = width / 7.4;

  return (
    <div
      className={`flex items-center select-none whitespace-nowrap ${className}`}
      style={{ width, fontSize, gap: fontSize * 0.4 }}
      role="img"
      aria-label={`${BRAND} ${PRODUCT}`}
    >
      <TrigoMark width={fontSize * 0.75} height={fontSize * 1.5} />
      <span className="leading-none tracking-tight">
        <span className="font-semibold text-logo-stroke">{BRAND}</span>{" "}
        <span className="font-normal text-logo-primary">{PRODUCT}</span>
      </span>
    </div>
  );
};

export default TrigoWhisprTextLogo;
