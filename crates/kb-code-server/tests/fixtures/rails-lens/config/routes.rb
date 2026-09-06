# frozen_string_literal: true

Rails.application.routes.draw do
  draw(:trade)
  devise_for :user
end
