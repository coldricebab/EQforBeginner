import unittest

from extract_rew_mdat import select_linear_grid_range


class LinearGridSelectionTests(unittest.TestCase):
    def test_nonzero_stored_origin_is_not_treated_as_bin_zero(self) -> None:
        start_hz = 20.1416015625
        step_hz = 0.3662109673023224
        first, last, selected_start = select_linear_grid_range(
            start_hz,
            step_hz,
            54_559,
            20.0,
            500.0,
        )

        self.assertEqual(first, 0)
        self.assertEqual(last, 1_310)
        self.assertEqual(selected_start, start_hz)

    def test_request_before_or_after_grid_is_clamped_or_rejected(self) -> None:
        self.assertEqual(
            select_linear_grid_range(20.0, 1.0, 10, 0.0, 22.0),
            (0, 2, 20.0),
        )
        with self.assertRaises(ValueError):
            select_linear_grid_range(20.0, 1.0, 10, 40.0, 50.0)


if __name__ == "__main__":
    unittest.main()
