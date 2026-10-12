/** @type {import('tailwindcss').Config} */
export default {
  darkMode: "class",
  content: ["./index.html", "./src/**/*.{js,ts,jsx,tsx}"],
  theme: {
    extend: {
      colors: {
        brand: {
          50: "#eef6ff",
          100: "#d9ebff",
          200: "#bcdcff",
          300: "#8ec7ff",
          400: "#59a8ff",
          500: "#3187fc",
          600: "#1667f4",
          700: "#0f51e3",
          800: "#1242b8",
          900: "#153b8f",
          950: "#102554",
        },
      },
      fontFamily: {
        sans: ["Segoe UI Variable Text", "Segoe UI", "system-ui", "sans-serif"],
        mono: ["Cascadia Mono", "Consolas", "ui-monospace", "monospace"],
      },
      boxShadow: {
        panel: "0 1px 2px 0 rgb(0 0 0 / 0.06), 0 8px 24px -8px rgb(0 0 0 / 0.18)",
      },
    },
  },
  plugins: [require("@tailwindcss/forms")],
};
