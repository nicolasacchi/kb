# frozen_string_literal: true

class Shop::Order
  MAX_LINES = 50

  class Line
    def amount
      0
    end
  end

  attr_writer :placed_at
  alias_method :sum, :total

  delegate :name, to: :customer

  def lines
    @lines ||= []
  end

  def self.build(attrs)
    new(attrs)
  end

  private def internal_key
    "x"
  end

  def self.dynamic_finders(names)
    names.each { |n| define_method(n) { nil } }
  end
end
