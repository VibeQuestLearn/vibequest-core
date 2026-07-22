if (
  typeof process !== "undefined" ||
  typeof fetch !== "undefined" ||
  typeof require !== "undefined" ||
  typeof console !== "undefined"
) {
  throw new Error("VQ_HOST_GLOBAL_SENTINEL");
}
