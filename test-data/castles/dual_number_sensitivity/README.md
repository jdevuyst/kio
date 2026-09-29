# Dual-number sensitivity

This program uses second-order forward automatic differentiation to evaluate
three polynomial models of two variables. Each intermediate value carries its
value, both first derivatives, and the three independent entries of its
symmetric Hessian. Addition, subtraction, scaling, and multiplication propagate
these components, so the model functions contain only their original formulas.

The models are:

- **Elastic energy:** set `u = 2x - y` and `v = x + y`, then evaluate
  `(u² + uv + 3v²) / 2`. The transformed coordinates couple the two inputs even
  though the Hessian is constant.
- **Calibration objective:** square the residuals `x² + y - 1` and
  `x - y² + 2`, then add `xy / 2`. This produces a quartic objective whose
  gradient and Hessian vary with the operating point.
- **Coupled response:** evaluate `(x + y)³ - (x - y)² + xy(x - 2y)`. Its cubic
  interactions exercise mixed derivatives through several composed products.

There is no stdin. The entry module supplies two operating points and a
perturbation direction for each model, using integers and halves. These small
dyadic fixtures keep every intermediate calculation exactly representable in
binary64. The expected results come from expanding each polynomial and
differentiating its monomials independently of the forward-differentiation
implementation.

For each operating point, stdout prints the model value, gradient `(dx, dy)`,
and Hessian `(dxx, dxy; dxy, dyy)`. It also reports the directional slope
`gradient · direction` and curvature `directionᵀ Hessian direction`. The
direction is a displacement per unit step, not a normalized unit vector.

`sensitivity/jet.kio` implements the differentiation algebra and directional
queries. `sensitivity/models.kio` composes that algebra into the three models.
The reporting module accepts each model as a function value, seeds the two
independent inputs, and prints its calculated sensitivities through the exact
`testapi-float` host interface.

## What this adds to the corpus

The castle adds compositional numerical differentiation: a fixed-dimensional
labeled product propagates first and second derivatives through higher-order
model evaluation. It combines nonlinear polynomial composition, mixed partial
derivatives, and directional curvature using only float arithmetic and output,
with several independently checkable operating points.
