# frozen_string_literal: true

module Shop
  class Order < ApplicationRecord
    include Payable
    prepend Auditable
    extend Findable

    TAX_RATE = 0.22
    DYNAMIC_FIELDS = %i[shipping handling].freeze
    attr_accessor(*DYNAMIC_FIELDS)

    attr_accessor :customer_ref
    attr_reader :placed_at

    class << self
      def open_orders
        all
      end

      private

      def hidden_scope
        none
      end
    end

    def total
      lines.sum(&:amount)
    end

    def summary
      "order"
    end

    def audit_key
      "order"
    end
    private :audit_key

    define_method(:legacy_total) { total }

    def method_missing(name, *args)
      super
    end

    def reset!
      send(:recompute)
    end

    def self.decorate!
      class_eval { attr_reader :decorated }
    end

    private

    def recompute
      total
    end
  end
end
