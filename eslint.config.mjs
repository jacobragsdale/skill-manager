// @ts-check

import eslint from "@eslint/js";
import eslintConfigPrettier from "eslint-config-prettier/flat";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import tseslint from "typescript-eslint";

const tsconfigRootDir = dirname(fileURLToPath(import.meta.url));

export default tseslint.config(
  { ignores: ["coverage/**", "dist/**"] },
  { linterOptions: { reportUnusedDisableDirectives: "error", reportUnusedInlineConfigs: "error" } },
  {
    files: ["**/*.{ts,tsx}"],
    extends: [eslint.configs.recommended, ...tseslint.configs.strictTypeChecked, ...tseslint.configs.stylisticTypeChecked],
    languageOptions: { parserOptions: { projectService: true, tsconfigRootDir } },
    rules: {
      complexity: ["error", 15],
      curly: ["error", "all"],
      eqeqeq: ["error", "always"],
      "guard-for-in": "error",
      "no-param-reassign": ["error", { props: true }],
      "no-promise-executor-return": "error",
      "no-restricted-syntax": ["error", { selector: "SequenceExpression", message: "Do not use the comma operator; split the expressions into explicit statements." }],
      "no-return-assign": ["error", "always"],
      "prefer-object-has-own": "error",
      "require-atomic-updates": "error",
      "@typescript-eslint/consistent-type-exports": "error",
      "@typescript-eslint/consistent-type-imports": ["error", { prefer: "type-imports", fixStyle: "separate-type-imports" }],
      "@typescript-eslint/explicit-function-return-type": ["error", { allowConciseArrowFunctionExpressionsStartingWithVoid: false, allowExpressions: true, allowTypedFunctionExpressions: true }],
      "@typescript-eslint/explicit-member-accessibility": ["error", { accessibility: "explicit", overrides: { constructors: "no-public" } }],
      "@typescript-eslint/explicit-module-boundary-types": "error",
      "@typescript-eslint/no-floating-promises": ["error", { checkThenables: true, ignoreIIFE: false, ignoreVoid: false }],
      "@typescript-eslint/no-import-type-side-effects": "error",
      "@typescript-eslint/no-loop-func": "error",
      "@typescript-eslint/no-unsafe-type-assertion": "error",
      "@typescript-eslint/prefer-readonly": "error",
      "@typescript-eslint/require-array-sort-compare": ["error", { ignoreStringArrays: false }],
      "@typescript-eslint/strict-boolean-expressions": [
        "error",
        {
          allowAny: false,
          allowNullableBoolean: false,
          allowNullableEnum: false,
          allowNullableNumber: false,
          allowNullableObject: false,
          allowNullableString: false,
          allowNumber: false,
          allowString: false
        }
      ],
      "@typescript-eslint/strict-void-return": "error",
      "@typescript-eslint/switch-exhaustiveness-check": ["error", { allowDefaultCaseForExhaustiveSwitch: false, considerDefaultExhaustiveForUnions: false, requireDefaultForNonUnion: true }]
    }
  },
  eslintConfigPrettier
);
