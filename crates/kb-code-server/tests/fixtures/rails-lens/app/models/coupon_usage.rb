# frozen_string_literal: true

class CouponUsage < ApplicationRecord
  include Discountable
  include Loggable
  include some_dynamic_module

  belongs_to :order, class_name: 'Purchase'
  has_many :orders
  has_many some_dynamic_association

  scope :active, -> { where(active: true) }
  scope dynamic_scope_name, -> { where(active: false) }

  before_save :normalize!
  before_create { raise ArgumentError if bogus? }

  validates :state, :status, presence: true
  validate dynamic_validator_method

  delegate :next_day_count, to: :order
  delegate to: :order
end
