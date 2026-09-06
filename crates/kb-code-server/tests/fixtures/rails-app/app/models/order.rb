# frozen_string_literal: true

class Order < ApplicationRecord
  include Discountable

  has_many :line_items
  validates :state, presence: true
  scope :recent, -> { order(created_at: :desc) }
  before_save :normalize_state

  def normalize_state
    self.state = state.to_s.downcase
  end
end
