# frozen_string_literal: true

class ApplicationRecord < ActiveRecord::Base
  self.abstract_class = true

  def self.audited?
    false
  end

  def touch_audit
    true
  end
end
