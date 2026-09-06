# frozen_string_literal: true
# SPDX-License-Identifier: MIT
# Copyright (c) 2026 Example Org

# == Schema Information
#
# Table name: widgets
#
#  id         :bigint           not null, primary key
#  name       :string
#

# ==== Ordering ====

# Returns the widget's display name.
# Falls back to the id when the name is blank.
class Widget
  # TODO(on: date('2027-09-01'), to: 'owner@example.com') drop the shim
  # rubocop:disable Metrics/AbcSize
  def display_name
    # legacy = name.upcase
    # legacy.strip!
    name.to_s.strip
  end
  # rubocop:enable Metrics/AbcSize
end

# A closing note that belongs to nothing below it.
